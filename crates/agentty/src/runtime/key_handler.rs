use std::io;

use ag_session::{CreateSessionMode, CreateSessionRequest};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use tracing::warn;

use crate::app::App;
use crate::domain::orchestration::IntegrationApproach;
use crate::domain::session::{SessionId, can_append_session_to_stack};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, ConfirmationViewMode, DiffSidebarFocus,
};
use crate::runtime::mode::confirmation::ConfirmationDecision;
use crate::runtime::{EventResult, PresentationState, backend_err, mode};

/// Routes key events to the active mode handler and returns the next runtime
/// action.
///
/// Successful handlers mark the app dirty so the next loop iteration renders
/// the updated UI state.
pub(crate) async fn handle_key_event<B: Backend>(
    app: &mut App,
    presentation: &PresentationState,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // Further input keeps background creation from interrupting the user.

    let result = if let AppMode::Confirmation {
        selected_confirmation_index,
        ..
    } = &mut app.mode
    {
        let decision = mode::confirmation::handle(selected_confirmation_index, key);

        handle_confirmation_decision(app, decision).await
    } else if matches!(app.mode, AppMode::SessionCreation { .. }) {
        handle_session_creation_key(app, key).await
    } else if matches!(app.mode, AppMode::ProjectSwitcher { .. }) {
        handle_project_switcher_key(app, key).await
    } else if matches!(app.mode, AppMode::LaunchConfigurationSelector { .. }) {
        handle_launch_configuration_selector_key(app, key).await
    } else if matches!(app.mode, AppMode::PublishBranchInput { .. }) {
        Ok(handle_publish_branch_input_key(app, key).await)
    } else {
        match &app.mode {
            AppMode::List => mode::list::handle(app, key).await,
            AppMode::SessionCreation { .. } => {
                unreachable!("session creation mode is handled before dispatch matching")
            }
            AppMode::StackAppendParentSelection { .. } => {
                handle_stack_append_parent_key(app, key).await
            }
            AppMode::PreCommitHookWarning { .. } => {
                Ok(handle_pre_commit_hook_warning_key(app, key))
            }
            AppMode::ProjectSwitcher { .. } => {
                unreachable!("project switcher mode is handled before dispatch matching")
            }
            AppMode::SyncBlockedPopup { .. } => Ok(mode::sync_blocked::handle(app, key)),
            AppMode::ViewInfoPopup { .. } => Ok(handle_view_info_popup_key(app, key)),
            AppMode::Confirmation { .. } => {
                unreachable!("confirmation mode is handled before dispatch matching")
            }
            AppMode::View { .. } => {
                mode::session_view::handle_with_cache(
                    app,
                    presentation.render_cache_store(),
                    terminal,
                    key,
                )
                .await
            }
            AppMode::Prompt { .. } => {
                mode::prompt::handle_with_cache(
                    app,
                    presentation.render_cache_store(),
                    terminal,
                    key,
                )
                .await
            }
            AppMode::Question { .. } => handle_question_key(app, presentation, terminal, key).await,
            AppMode::DiffLoading { .. } => Ok(mode::diff::handle_loading(app, key)),
            AppMode::Diff {
                review_comments: Some(review_comments),
                ..
            } if review_comments.sidebar_focus == DiffSidebarFocus::Comments
                && !mode::diff::should_submit_line_comments(app, key)
                && !matches!(key.code, KeyCode::Char('?' | 'q')) =>
            {
                handle_review_comment_key(app, presentation, terminal, key).await
            }
            AppMode::Diff { .. } => {
                let size = terminal.size().map_err(backend_err)?;
                let terminal_rect = Rect::new(0, 0, size.width, size.height);
                let content_area = content_area_for_terminal(terminal_rect);
                let submit_line_comments = mode::diff::should_submit_line_comments(app, key);

                let result = mode::diff::handle_with_cache(
                    app,
                    presentation.render_cache_store(),
                    content_area,
                    key,
                );
                if submit_line_comments && matches!(app.mode, AppMode::Prompt { .. }) {
                    mode::prompt::submit_current_text_prompt(app).await;
                }

                Ok(result)
            }
            AppMode::Help { .. } => Ok(mode::help::handle(app, key)),
            AppMode::LaunchConfigurationSelector { .. } => {
                unreachable!(
                    "launch-configuration selector mode is handled before dispatch matching"
                )
            }
            AppMode::PublishBranchInput { .. } => {
                unreachable!("publish-branch input mode is handled before dispatch matching")
            }
        }
    };

    if result.is_ok() {
        app.mark_dirty();
    }

    result
}

/// Resolves the full terminal area and routes one clarification-question key
/// event through the shared render cache.
async fn handle_question_key<B: Backend>(
    app: &mut App,
    presentation: &PresentationState,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let size = terminal.size().map_err(backend_err)?;
    let terminal_rect = Rect::new(0, 0, size.width, size.height);

    Ok(mode::question::handle_with_cache(
        app,
        presentation.render_cache_store(),
        terminal_rect,
        key,
    )
    .await)
}

/// Resolves the page content area and routes one review-comment key event.
async fn handle_review_comment_key<B: Backend>(
    app: &mut App,
    presentation: &PresentationState,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let size = terminal.size().map_err(backend_err)?;
    let terminal_rect = Rect::new(0, 0, size.width, size.height);
    let content_area = content_area_for_terminal(terminal_rect);

    Ok(mode::review_comment::handle_with_cache(
        app,
        presentation.render_cache_store(),
        content_area,
        key,
    )
    .await)
}

/// Returns the central content area after removing the global status and
/// footer bars from the full terminal rectangle.
fn content_area_for_terminal(terminal_rect: Rect) -> Rect {
    let outer_chunks = Layout::default()
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(terminal_rect);

    outer_chunks[1]
}

/// Handles key input while the session creation selector is visible.
async fn handle_session_creation_key(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::List;
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            select_previous_session_creation_option(app);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            select_next_session_creation_option(app);
        }
        KeyCode::Enter => {
            create_selected_session(app).await?;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Updates the highlighted option in the session creation selector.
fn update_session_creation_selection(app: &mut App, selected_option_index: usize) {
    let mut selected_option_index = selected_option_index.min(4);
    while selected_option_index > 0
        && !session_creation_option_is_enabled(app, selected_option_index)
    {
        selected_option_index = selected_option_index.saturating_sub(1);
    }

    if let AppMode::SessionCreation {
        selected_option_index: current_index,
    } = &mut app.mode
    {
        *current_index = selected_option_index;
    }
}

/// Moves to the previous enabled session-creation option.
fn select_previous_session_creation_option(app: &mut App) {
    let current_index = current_session_creation_selection(app);
    let previous_index = (0..current_index)
        .rev()
        .find(|option_index| session_creation_option_is_enabled(app, *option_index))
        .unwrap_or(current_index);
    update_session_creation_selection(app, previous_index);
}

/// Moves to the next enabled session-creation option.
fn select_next_session_creation_option(app: &mut App) {
    let current_index = current_session_creation_selection(app);
    let next_index = ((current_index + 1)..=4)
        .find(|option_index| session_creation_option_is_enabled(app, *option_index))
        .unwrap_or(current_index);
    update_session_creation_selection(app, next_index);
}

/// Returns whether one creation-selector row can currently be chosen.
fn session_creation_option_is_enabled(app: &App, option_index: usize) -> bool {
    match option_index {
        0..=2 => true,
        3 => selected_stacked_parent_session_id(app).is_some(),
        4 => selected_stack_append_session_id(app).is_some(),
        _ => false,
    }
}

/// Creates the selected session type and opens its prompt composer.
async fn create_selected_session(app: &mut App) -> io::Result<()> {
    let selected_option_index = current_session_creation_selection(app);
    let mode = match selected_option_index {
        0 => CreateSessionMode::Regular,
        1 => CreateSessionMode::Draft,
        2 => CreateSessionMode::Orchestrator,
        3 => {
            let Some(parent_session_id) = selected_stacked_parent_session_id(app) else {
                return Ok(());
            };

            CreateSessionMode::Stacked { parent_session_id }
        }
        4 => {
            let Some(session_id) = selected_stack_append_session_id(app) else {
                return Ok(());
            };
            app.mode = AppMode::StackAppendParentSelection {
                selected_parent_index: 0,
                session_id,
            };

            return Ok(());
        }
        _ => return Ok(()),
    };
    let project_id = app.active_project_id();
    app.start_session_creation(
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode,
            project_id,
        },
        None,
    )
    .await;

    Ok(())
}

/// Handles the advisory shown before session-type selection.
fn handle_pre_commit_hook_warning_key(app: &mut App, key: KeyEvent) -> EventResult {
    match key.code {
        KeyCode::Enter => {
            app.mode = AppMode::SessionCreation {
                selected_option_index: 0,
            };
        }
        KeyCode::Esc => app.mode = AppMode::List,
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = AppMode::List;
        }
        _ => {}
    }

    EventResult::Continue
}

/// Returns the current highlighted session-creation option.
fn current_session_creation_selection(app: &App) -> usize {
    match app.mode {
        AppMode::SessionCreation {
            selected_option_index,
        } => selected_option_index,
        _ => 0,
    }
}

/// Returns the selected session id when it can parent a stacked draft.
fn selected_stacked_parent_session_id(app: &App) -> Option<SessionId> {
    app.selected_session()
        .filter(|session| app.sessions.can_create_stacked_child(&session.id))
        .map(|session| session.id.clone())
}

/// Returns the selected review-ready session when it has an eligible parent.
fn selected_stack_append_session_id(app: &App) -> Option<SessionId> {
    let selected_session = app.selected_session()?;
    app.sessions
        .sessions()
        .iter()
        .any(|candidate| {
            can_append_session_to_stack(
                app.sessions.sessions(),
                selected_session.id.as_str(),
                candidate.id.as_str(),
            )
        })
        .then(|| selected_session.id.clone())
}

/// Handles navigation and confirmation in the stack-parent selector.
async fn handle_stack_append_parent_key(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::SessionCreation {
                selected_option_index: 4,
            };
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            update_stack_append_parent_selection(app, true);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            update_stack_append_parent_selection(app, false);
        }
        KeyCode::Enter => append_session_to_selected_stack(app).await,
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Moves the highlighted eligible parent up or down by one row.
fn update_stack_append_parent_selection(app: &mut App, move_up: bool) {
    let AppMode::StackAppendParentSelection {
        selected_parent_index,
        session_id,
    } = &app.mode
    else {
        return;
    };
    let parent_count = stack_append_parent_session_ids(app, session_id).len();
    let updated_index = if move_up {
        selected_parent_index.saturating_sub(1)
    } else {
        selected_parent_index
            .saturating_add(1)
            .min(parent_count.saturating_sub(1))
    };

    if let AppMode::StackAppendParentSelection {
        selected_parent_index,
        ..
    } = &mut app.mode
    {
        *selected_parent_index = updated_index;
    }
}

/// Returns eligible parent identifiers in visible session order.
fn stack_append_parent_session_ids(app: &App, session_id: &SessionId) -> Vec<SessionId> {
    app.sessions
        .sessions()
        .iter()
        .filter(|candidate| {
            can_append_session_to_stack(
                app.sessions.sessions(),
                session_id.as_str(),
                candidate.id.as_str(),
            )
        })
        .map(|session| session.id.clone())
        .collect()
}

/// Moves the source session beneath the highlighted parent and starts its
/// synchronization.
async fn append_session_to_selected_stack(app: &mut App) {
    let AppMode::StackAppendParentSelection {
        selected_parent_index,
        session_id,
    } = &app.mode
    else {
        return;
    };
    let session_id = session_id.clone();
    let parent_session_id = stack_append_parent_session_ids(app, &session_id)
        .get(*selected_parent_index)
        .cloned();
    app.mode = AppMode::List;

    let Some(parent_session_id) = parent_session_id else {
        return;
    };
    if let Err(error) = app
        .append_session_to_stack(session_id.as_str(), parent_session_id.as_str())
        .await
    {
        app.mode = AppMode::SyncBlockedPopup {
            default_branch: None,
            is_loading: false,
            message: error.to_string(),
            project_name: None,
            title: "Append to stack failed".to_string(),
        };
    }
}

/// Handles key input while the MRU project switcher popup is visible.
async fn handle_project_switcher_key(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::List;
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            update_project_switcher_selection(
                app,
                current_project_switcher_selection(app).saturating_sub(1),
            );
        }
        KeyCode::Down | KeyCode::Char('j') => {
            update_project_switcher_selection(
                app,
                current_project_switcher_selection(app).saturating_add(1),
            );
        }
        KeyCode::Enter => {
            switch_to_selected_switcher_project(app).await;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Switches the active project to the highlighted MRU row and returns to the
/// sessions list.
///
/// Selecting the already-active project closes the popup without a switch. A
/// failed switch replaces the popup with the list informational popup so the
/// user sees why the project did not change instead of a no-op.
async fn switch_to_selected_switcher_project(app: &mut App) {
    let selected_project = app
        .projects
        .mru_project_items()
        .get(current_project_switcher_selection(app))
        .map(|project_item| {
            (
                project_item.project.id,
                project_item.project.display_label(),
            )
        });
    app.mode = AppMode::List;

    let Some((selected_project_id, selected_project_label)) = selected_project else {
        return;
    };

    if selected_project_id == app.active_project_id() {
        return;
    }

    if let Err(error) = app.switch_project(selected_project_id).await {
        app.mode = AppMode::SyncBlockedPopup {
            default_branch: None,
            is_loading: false,
            message: error.to_string(),
            project_name: Some(selected_project_label),
            title: "Project switch failed".to_string(),
        };
    }
}

/// Clamps and stores the highlighted row in the project switcher popup.
fn update_project_switcher_selection(app: &mut App, selected_option_index: usize) {
    let max_option_index = app.projects.mru_project_items().len().saturating_sub(1);

    if let AppMode::ProjectSwitcher {
        selected_option_index: current_index,
    } = &mut app.mode
    {
        *current_index = selected_option_index.min(max_option_index);
    }
}

/// Returns the currently highlighted project switcher row.
fn current_project_switcher_selection(app: &App) -> usize {
    match app.mode {
        AppMode::ProjectSwitcher {
            selected_option_index,
        } => selected_option_index,
        _ => 0,
    }
}

/// Handles key input while a session-scoped informational popup is visible.
fn handle_view_info_popup_key(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::ViewInfoPopup {
        is_loading,
        restore_view,
        ..
    } = &app.mode
    else {
        return EventResult::Continue;
    };

    if *is_loading {
        return EventResult::Continue;
    }

    match key.code {
        KeyCode::Enter | KeyCode::Esc => {
            app.mode = restore_view.clone().into_view_mode();
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.mode = restore_view.clone().into_view_mode();
        }
        _ => {}
    }

    EventResult::Continue
}

/// Handles key input while the publish-branch input overlay is visible.
///
/// Only `Esc` cancels the overlay. Plain character keys continue to edit the
/// branch name so session-view shortcuts like `q` and `p` do not leak through
/// while the text field has focus.
async fn handle_publish_branch_input_key(app: &mut App, key: KeyEvent) -> EventResult {
    let publish_branch_input =
        PublishBranchInputModeState::from_mode(std::mem::replace(&mut app.mode, AppMode::List));
    let input_locked = publish_branch_input.locked_upstream_ref.is_some();

    match key.code {
        KeyCode::Esc => {
            app.mode = publish_branch_input.restore_view.into_view_mode();
        }
        KeyCode::Enter => {
            let remote_branch_name = if input_locked {
                Some(publish_branch_input.input.text().trim().to_string())
            } else {
                (!publish_branch_input.input.text().trim().is_empty())
                    .then(|| publish_branch_input.input.text().trim().to_string())
            };
            let session_id = publish_branch_input.restore_view.session_id.clone();

            app.start_publish_branch_action(
                publish_branch_input.restore_view,
                &session_id,
                publish_branch_input.publish_branch_action,
                remote_branch_name,
            )
            .await;
        }
        _ if !input_locked => {
            app.mode = if let Some(command) = mode::input_key::command_for_key(
                key,
                mode::input_key::InputCapabilities::SINGLE_LINE,
            ) {
                publish_branch_input.apply_input_edit(|input| {
                    input.apply(command);
                })
            } else {
                publish_branch_input.into_mode()
            };
        }
        _ => {
            app.mode = publish_branch_input.into_mode();
        }
    }

    EventResult::Continue
}

/// Captures `AppMode::PublishBranchInput` fields so key handlers can rebuild
/// the overlay consistently after input edits.
struct PublishBranchInputModeState {
    default_branch_name: String,
    input: crate::domain::input::InputState,
    locked_upstream_ref: Option<String>,
    publish_branch_action: crate::domain::session::PublishBranchAction,
    restore_view: ConfirmationViewMode,
}

impl PublishBranchInputModeState {
    /// Extracts publish-branch overlay fields from an app mode value.
    fn from_mode(mode: AppMode) -> Self {
        let AppMode::PublishBranchInput {
            default_branch_name,
            input,
            locked_upstream_ref,
            publish_branch_action,
            restore_view,
        } = mode
        else {
            unreachable!("mode must be publish-branch input in this handler");
        };

        Self {
            default_branch_name,
            input,
            locked_upstream_ref,
            publish_branch_action,
            restore_view,
        }
    }

    /// Applies one input edit and rebuilds the publish-branch overlay mode.
    fn apply_input_edit(
        mut self,
        edit: impl FnOnce(&mut crate::domain::input::InputState),
    ) -> AppMode {
        edit(&mut self.input);

        self.into_mode()
    }

    /// Rebuilds `AppMode::PublishBranchInput` from the stored overlay fields.
    fn into_mode(self) -> AppMode {
        AppMode::PublishBranchInput {
            default_branch_name: self.default_branch_name,
            input: self.input,
            locked_upstream_ref: self.locked_upstream_ref,
            publish_branch_action: self.publish_branch_action,
            restore_view: self.restore_view,
        }
    }
}

/// Handles key input while the app is in launch-configuration selector overlay
/// mode.
async fn handle_launch_configuration_selector_key(
    app: &mut App,
    key: KeyEvent,
) -> io::Result<EventResult> {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    let AppMode::LaunchConfigurationSelector {
        commands,
        restore_view,
        selected_command_index,
    } = mode
    else {
        unreachable!("mode must be launch-configuration selector in this handler");
    };

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = restore_view.into_view_mode();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.mode = AppMode::LaunchConfigurationSelector {
                selected_command_index: next_launch_configuration_index(
                    selected_command_index,
                    &commands,
                ),
                commands,
                restore_view,
            };
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.mode = AppMode::LaunchConfigurationSelector {
                selected_command_index: previous_launch_configuration_index(
                    selected_command_index,
                    &commands,
                ),
                commands,
                restore_view,
            };
        }
        KeyCode::Enter => {
            let selected_launch_configuration = commands
                .get(selected_command_index)
                .map(std::string::String::as_str);
            app.mode = restore_view.into_view_mode();
            app.open_session_worktree_in_tmux_with_command(selected_launch_configuration)
                .await;
        }
        _ => {
            app.mode = AppMode::LaunchConfigurationSelector {
                commands,
                restore_view,
                selected_command_index,
            };
        }
    }

    Ok(EventResult::Continue)
}

/// Returns the next command index with wrap-around.
fn next_launch_configuration_index(current_index: usize, commands: &[String]) -> usize {
    if commands.is_empty() {
        return 0;
    }

    (current_index + 1) % commands.len()
}

/// Returns the previous command index with wrap-around.
fn previous_launch_configuration_index(current_index: usize, commands: &[String]) -> usize {
    if commands.is_empty() {
        return 0;
    }

    if current_index == 0 {
        commands.len() - 1
    } else {
        current_index - 1
    }
}

/// Applies the semantic result of a generic confirmation interaction.
async fn handle_confirmation_decision(
    app: &mut App,
    decision: ConfirmationDecision,
) -> io::Result<EventResult> {
    match decision {
        ConfirmationDecision::Confirm => handle_confirmation_confirm(app).await,
        ConfirmationDecision::Reject => handle_confirmation_reject(app).await,
        ConfirmationDecision::Cancel => {
            app.mode = confirmation_cancel_mode(&app.mode);

            Ok(EventResult::Continue)
        }
        ConfirmationDecision::Continue => Ok(EventResult::Continue),
    }
}

/// Resolves target mode for `Cancel` in confirmation overlays.
fn confirmation_cancel_mode(mode: &AppMode) -> AppMode {
    if let AppMode::Confirmation {
        confirmation_intent:
            ConfirmationIntent::ContinueSession
            | ConfirmationIntent::ForkSession
            | ConfirmationIntent::MergeSession
            | ConfirmationIntent::RegenerateReview
            | ConfirmationIntent::DetachManagedSession
            | ConfirmationIntent::OpenManagedWorktree
            | ConfirmationIntent::ChooseIntegrationApproach,
        restore_view: Some(restore_view),
        ..
    } = mode
    {
        return restore_view.clone().into_view_mode();
    }

    AppMode::List
}

/// Resolves a positive confirmation by dispatching the configured action
/// intent.
async fn handle_confirmation_confirm(app: &mut App) -> io::Result<EventResult> {
    let (confirmation_intent, confirmation_session_id, restore_view) = match &app.mode {
        AppMode::Confirmation {
            confirmation_intent,
            restore_view,
            session_id,
            ..
        } => (
            *confirmation_intent,
            session_id.clone(),
            restore_view.clone(),
        ),
        _ => return Ok(EventResult::Continue),
    };

    match confirmation_intent {
        ConfirmationIntent::Quit => {
            app.mode = AppMode::List;

            Ok(EventResult::Quit)
        }
        ConfirmationIntent::CancelSession => {
            handle_cancel_session_confirmation(app, confirmation_session_id).await
        }
        ConfirmationIntent::ContinueSession => {
            handle_continue_session_confirmation(app, confirmation_session_id, restore_view).await
        }
        ConfirmationIntent::ForkSession => {
            handle_fork_session_confirmation(app, confirmation_session_id, restore_view).await
        }
        ConfirmationIntent::MergeSession => {
            handle_merge_confirmation(app, confirmation_session_id, restore_view).await
        }
        ConfirmationIntent::RegenerateReview => Ok(handle_regenerate_review_confirmation(
            app,
            confirmation_session_id,
            restore_view,
        )),
        ConfirmationIntent::DetachManagedSession => {
            handle_detach_managed_session_confirmation(app, confirmation_session_id, restore_view)
                .await
        }
        ConfirmationIntent::OpenManagedWorktree => {
            handle_open_managed_worktree_confirmation(app, confirmation_session_id, restore_view)
                .await
        }
        ConfirmationIntent::ChooseIntegrationApproach => {
            handle_integration_approach_confirmation(
                app,
                confirmation_session_id,
                restore_view,
                IntegrationApproach::LocalMerge,
            )
            .await
        }
    }
}

/// Opens a managed worker worktree after the user acknowledges write access.
async fn handle_open_managed_worktree_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(restore_view) = restore_view else {
        app.mode = AppMode::List;

        return Ok(EventResult::Continue);
    };
    if confirmation_session_id.as_ref() != Some(&restore_view.session_id) {
        app.mode = restore_view.into_view_mode();

        return Ok(EventResult::Continue);
    }

    mode::session_view::open_worktree_for_view_session(app, restore_view).await;

    Ok(EventResult::Continue)
}

/// Resolves the second binary choice, which only has distinct semantics for
/// orchestration integration; ordinary confirmations treat it as dismissal.
async fn handle_confirmation_reject(app: &mut App) -> io::Result<EventResult> {
    let AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
        restore_view,
        session_id,
        ..
    } = &app.mode
    else {
        app.mode = confirmation_cancel_mode(&app.mode);

        return Ok(EventResult::Continue);
    };
    let confirmation_session_id = session_id.clone();
    let restore_view = restore_view.clone();

    handle_integration_approach_confirmation(
        app,
        confirmation_session_id,
        restore_view,
        IntegrationApproach::ReviewRequest,
    )
    .await
}

/// Advances one verified campaign using the selected integration destination.
async fn handle_integration_approach_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
    integration_approach: IntegrationApproach,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };
    app.approve_orchestration(&session_id, Some(integration_approach))
        .await;
    app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

    Ok(EventResult::Continue)
}

/// Transfers one confirmed managed worker to ordinary user ownership.
async fn handle_detach_managed_session_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };
    app.detach_managed_child(&session_id).await;
    app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

    Ok(EventResult::Continue)
}

/// Cancels the confirmed cancelable session, when still present, and returns
/// to list mode.
async fn handle_cancel_session_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
) -> io::Result<EventResult> {
    app.mode = AppMode::List;

    let Some(session_id) = confirmation_session_id else {
        return Ok(EventResult::Continue);
    };
    let service = app.session_service();
    let request = service.cancel_session(&session_id);
    if let Err(error) = app.drive_session_request(request).await {
        warn!(
            session_id = %session_id,
            error = %error,
            "failed to cancel confirmed session"
        );
    }

    Ok(EventResult::Continue)
}

/// Creates a continuation draft for the confirmed terminal session and opens
/// its prompt composer.
async fn handle_continue_session_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };

    if let Err(error) = app.continue_terminal_session(&session_id).await {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);
        app.append_output_for_session(&session_id, &TranscriptNotice::ContinueError.format(error))
            .await;
    }

    Ok(EventResult::Continue)
}

/// Creates a fork of the confirmed source session and opens the forked
/// session view.
async fn handle_fork_session_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };

    if let Err(error) = app.fork_session(&session_id).await {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);
        app.append_output_for_session(&session_id, &TranscriptNotice::ForkError.format(error))
            .await;
    }

    Ok(EventResult::Continue)
}

/// Restores view mode and attempts to add confirmed session to merge queue.
async fn handle_merge_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

    let Some(session_id) = confirmation_session_id else {
        return Ok(EventResult::Continue);
    };
    let service = app.session_service();
    let request = service.merge_session(&session_id);
    if let Err(error) = app.drive_session_request(request).await {
        app.append_output_for_session(&session_id, &TranscriptNotice::MergeError.format(error))
            .await;
    }

    Ok(EventResult::Continue)
}

/// Clears focused review cache state, requests the diff in the background,
/// then restores session view with responsive loading state.
fn handle_regenerate_review_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> EventResult {
    let Some(session_id) = confirmation_session_id else {
        app.mode = AppMode::List;

        return EventResult::Continue;
    };

    app.clear_review_output(session_id.as_str());

    if !app
        .sessions
        .sessions()
        .iter()
        .any(|session| session.id == session_id)
    {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return EventResult::Continue;
    }

    app.start_manual_review_diff_load(&session_id);

    let view_mode = restore_view.unwrap_or(ConfirmationViewMode {
        scroll_offset: None,
        session_id,
    });
    app.mode = view_mode.into_view_mode();

    EventResult::Continue
}

#[cfg(test)]
#[path = "key_handler_test.rs"]
mod tests;
