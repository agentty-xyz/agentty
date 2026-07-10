use std::io;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use tracing::warn;

use crate::app::{App, ReviewCacheEntry, diff_content_hash};
use crate::domain::session::SessionId;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::presentation::app_mode::{AppMode, ConfirmationIntent, ConfirmationViewMode};
use crate::runtime::mode::confirmation::ConfirmationDecision;
use crate::runtime::{EventResult, backend_err, mode};
use crate::ui::RenderCacheStore;

/// Routes key events to the active mode handler and returns the next runtime
/// action.
///
/// Successful handlers mark the app dirty so the next loop iteration renders
/// the updated UI state.
pub(crate) async fn handle_key_event<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
    render_cache_store: &RenderCacheStore,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let result = if let AppMode::Confirmation {
        selected_confirmation_index,
        ..
    } = &mut app.mode
    {
        let decision = mode::confirmation::handle(selected_confirmation_index, key);

        handle_confirmation_decision(app, decision).await
    } else if matches!(app.mode, AppMode::SessionCreation { .. }) {
        handle_session_creation_key(app, key).await
    } else if matches!(app.mode, AppMode::LaunchConfigurationSelector { .. }) {
        handle_launch_configuration_selector_key(app, key).await
    } else if matches!(app.mode, AppMode::PublishBranchInput { .. }) {
        Ok(handle_publish_branch_input_key(app, key))
    } else {
        match &app.mode {
            AppMode::List => mode::list::handle(app, key).await,
            AppMode::ReviewDetail { .. } => {
                let size = terminal.size().map_err(backend_err)?;
                let terminal_rect = Rect::new(0, 0, size.width, size.height);
                let content_area = content_area_for_terminal(terminal_rect);

                Ok(mode::review_detail::handle(
                    app,
                    content_area,
                    key,
                    render_cache_store,
                ))
            }
            AppMode::SessionCreation { .. } => {
                unreachable!("session creation mode is handled before dispatch matching")
            }
            AppMode::SyncBlockedPopup { .. } => Ok(mode::sync_blocked::handle(app, key)),
            AppMode::ViewInfoPopup { .. } => Ok(handle_view_info_popup_key(app, key)),
            AppMode::Confirmation { .. } => {
                unreachable!("confirmation mode is handled before dispatch matching")
            }
            AppMode::View { .. } => {
                mode::session_view::handle(app, terminal, key, render_cache_store).await
            }
            AppMode::Prompt { .. } => mode::prompt::handle(app, terminal, key).await,
            AppMode::Question { .. } => {
                let size = terminal.size().map_err(backend_err)?;
                let terminal_rect = Rect::new(0, 0, size.width, size.height);

                Ok(mode::question::handle(app, terminal_rect, key, render_cache_store).await)
            }
            AppMode::Diff { .. } => {
                let size = terminal.size().map_err(backend_err)?;
                let terminal_rect = Rect::new(0, 0, size.width, size.height);
                let content_area = content_area_for_terminal(terminal_rect);

                Ok(mode::diff::handle(
                    app,
                    content_area,
                    key,
                    render_cache_store,
                ))
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
            update_session_creation_selection(app, 0);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            update_session_creation_selection(
                app,
                next_session_creation_selection(app).saturating_add(1),
            );
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
    let max_option_index = if selected_stacked_parent_session_id(app).is_some() {
        2
    } else {
        1
    };

    if let AppMode::SessionCreation {
        selected_option_index: current_index,
    } = &mut app.mode
    {
        *current_index = selected_option_index.min(max_option_index);
    }
}

/// Creates the selected session type and opens its prompt composer.
async fn create_selected_session(app: &mut App) -> io::Result<()> {
    let selected_option_index = current_session_creation_selection(app);
    let session_id = match selected_option_index {
        0 => app.create_session().await.map_err(io::Error::other)?,
        1 => app.create_draft_session().await.map_err(io::Error::other)?,
        2 => {
            let Some(parent_session_id) = selected_stacked_parent_session_id(app) else {
                return Ok(());
            };

            app.create_stacked_draft_session(parent_session_id.as_str())
                .await
                .map_err(io::Error::other)?
        }
        _ => return Ok(()),
    };
    mode::list::open_session_prompt(app, session_id);

    Ok(())
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

/// Returns the next selectable session-creation option for down navigation.
fn next_session_creation_selection(app: &App) -> usize {
    current_session_creation_selection(app)
}

/// Returns the selected session id when it can parent a stacked draft.
fn selected_stacked_parent_session_id(app: &App) -> Option<SessionId> {
    app.selected_session()
        .filter(|session| session.allows_stacked_child_creation())
        .map(|session| session.id.clone())
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
fn handle_publish_branch_input_key(app: &mut App, key: KeyEvent) -> EventResult {
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
            );
        }
        KeyCode::Left if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_left);
        }
        KeyCode::Right if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_right);
        }
        KeyCode::Up if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_up);
        }
        KeyCode::Down if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_down);
        }
        KeyCode::Home if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_home);
        }
        KeyCode::End if !input_locked => {
            app.mode =
                publish_branch_input.apply_input_edit(crate::domain::input::InputState::move_end);
        }
        KeyCode::Backspace if !input_locked => {
            app.mode = publish_branch_input
                .apply_input_edit(crate::domain::input::InputState::delete_backward);
        }
        KeyCode::Delete if !input_locked => {
            app.mode = publish_branch_input
                .apply_input_edit(crate::domain::input::InputState::delete_forward);
        }
        KeyCode::Char(character) if !input_locked && is_publish_branch_input_text_key(key) => {
            app.mode = publish_branch_input.apply_input_edit(|input| input.insert_char(character));
        }
        _ => {
            app.mode = publish_branch_input.into_mode();
        }
    }

    EventResult::Continue
}

/// Returns whether one key event should insert text into the publish-branch
/// input field.
///
/// Matches the prompt/question input policy: only plain and shifted
/// characters are treated as text input.
fn is_publish_branch_input_text_key(key: KeyEvent) -> bool {
    key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT
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
            | ConfirmationIntent::RegenerateReview,
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
        ConfirmationIntent::RegenerateReview => {
            handle_regenerate_review_confirmation(app, confirmation_session_id, restore_view).await
        }
    }
}

/// Cancels the confirmed cancelable session, when still present, and returns
/// to list mode.
async fn handle_cancel_session_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
) -> io::Result<EventResult> {
    app.mode = AppMode::List;

    if let Some(session_id) = confirmation_session_id
        && let Err(error) = app.cancel_session(&session_id).await
    {
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

    if let Some(session_id) = confirmation_session_id
        && let Err(error) = app.merge_session(&session_id).await
    {
        app.append_output_for_session(&session_id, &TranscriptNotice::MergeError.format(error))
            .await;
    }

    Ok(EventResult::Continue)
}

/// Clears focused review cache state and persisted review text, restarts
/// generation for the confirmed session, then restores session view with the
/// refreshed review state.
async fn handle_regenerate_review_confirmation(
    app: &mut App,
    confirmation_session_id: Option<SessionId>,
    restore_view: Option<ConfirmationViewMode>,
) -> io::Result<EventResult> {
    let Some(session_id) = confirmation_session_id else {
        app.mode = AppMode::List;

        return Ok(EventResult::Continue);
    };

    app.review_cache.remove(session_id.as_str());

    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id);
    let Some(session) = session else {
        app.mode = restore_view.map_or(AppMode::List, ConfirmationViewMode::into_view_mode);

        return Ok(EventResult::Continue);
    };

    let session_folder = session.folder.clone();
    let base_branch = session.base_branch.clone();

    let diff = app
        .services
        .git_client()
        .diff(session_folder.clone(), base_branch)
        .await
        .unwrap_or_else(|error| format!("Failed to run git diff: {error}"));

    if diff.trim().is_empty() || diff.starts_with("Failed to run git diff:") {
        let view_mode = restore_view.unwrap_or(ConfirmationViewMode {
            scroll_offset: None,
            session_id: session_id.clone(),
        });
        let diff_hash = diff_content_hash(&diff);
        let review_text = if diff.trim().is_empty() {
            "No diff changes found for review.".to_string()
        } else {
            diff
        };
        app.review_cache.insert(
            session_id,
            ReviewCacheEntry::Ready {
                diff_hash,
                text: review_text,
            },
        );
        app.mode = view_mode.into_view_mode();

        return Ok(EventResult::Continue);
    }

    let diff_hash = diff_content_hash(&diff);
    app.review_cache
        .insert(session_id.clone(), ReviewCacheEntry::Loading { diff_hash });
    let _ = app
        .services
        .db()
        .sessions()
        .update_session_focused_review(session_id.as_str(), None, None)
        .await;
    app.start_review_assist(session_id.as_str(), &session_folder, diff_hash, &diff);

    let view_mode = restore_view.unwrap_or(ConfirmationViewMode {
        scroll_offset: None,
        session_id,
    });
    app.mode = view_mode.into_view_mode();

    Ok(EventResult::Continue)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crossterm::event::KeyModifiers;
    use mockall::predicate::eq;

    use super::*;
    use crate::domain::session_message::SessionTranscript;
    use crate::infra::tmux::MockTmuxClient;
    use crate::presentation::app_mode::ConfirmationViewMode;

    /// Routes one test key through a fresh render-cache boundary.
    async fn handle_key_event<B: Backend>(
        app: &mut App,
        terminal: &mut Terminal<B>,
        key: KeyEvent,
    ) -> io::Result<EventResult>
    where
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let render_cache_store = RenderCacheStore::default();

        super::handle_key_event(app, terminal, key, &render_cache_store).await
    }

    fn session_replay_text(session: &crate::domain::session::Session) -> String {
        session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .unwrap_or_default()
    }

    #[test]
    fn test_content_area_for_terminal_excludes_global_bars() {
        // Arrange
        let terminal_rect = Rect::new(0, 0, 120, 30);

        // Act
        let content_area = content_area_for_terminal(terminal_rect);

        // Assert
        assert_eq!(content_area, Rect::new(0, 1, 120, 28));
    }

    #[tokio::test]
    async fn test_handle_session_creation_key_creates_regular_session() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::SessionCreation {
            selected_option_index: 0,
        };

        // Act
        let result = handle_session_creation_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert_eq!(app.sessions.sessions().len(), 1);
        assert!(!app.sessions.sessions()[0].is_draft_session());
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref session_id,
                scroll_offset: None,
                ..
            } if !session_id.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_session_creation_key_creates_draft_session() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::SessionCreation {
            selected_option_index: 0,
        };

        // Act
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .expect("failed to move selection");
        let result = handle_session_creation_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert_eq!(app.sessions.sessions().len(), 1);
        assert!(app.sessions.sessions()[0].is_draft_session());
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref session_id,
                scroll_offset: None,
                ..
            } if !session_id.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_session_creation_key_escape_returns_to_list() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::SessionCreation {
            selected_option_index: 0,
        };

        // Act
        let result =
            handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_view_info_popup_key_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::ViewInfoPopup {
            is_loading: false,
            loading_label: "Refreshing review request...".to_string(),
            message: "Review request refreshed.".to_string(),
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(2),
                session_id: "session-id".into(),
            },
            title: "Review request refreshed".to_string(),
        };

        // Act
        let event_result =
            handle_view_info_popup_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(2),
                ..
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_quits_when_no_session_context() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::Quit,
            confirmation_message: "Quit agentty?".to_string(),
            confirmation_title: "Confirm Quit".to_string(),
            restore_view: None,
            session_id: None,
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Quit)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_returns_to_list() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::Quit,
            confirmation_message: "Quit agentty?".to_string(),
            confirmation_title: "Confirm Quit".to_string(),
            restore_view: None,
            session_id: None,
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_cancels_session_when_context_exists() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        crate::test_support::persist_session_status_for_test(
            &mut app,
            &session_id,
            crate::domain::session::Status::Review,
        )
        .await;
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::CancelSession,
            confirmation_message: "Cancel session \"test\"?".to_string(),
            confirmation_title: "Confirm Cancel".to_string(),
            restore_view: None,
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
        app.sessions.sync_from_handles();
        assert!(matches!(
            app.sessions.sessions().first(),
            Some(session) if session.id == session_id
                && session.status == crate::domain::session::Status::Canceled
        ));
    }

    #[tokio::test]
    async fn test_handle_key_event_routes_done_session_continue_shortcut() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let source_session_id = app
            .create_session()
            .await
            .expect("failed to create source session");
        app.services
            .db()
            .sessions()
            .update_session_merged_commit_hash(&source_session_id, Some("abc1234".to_string()))
            .await
            .expect("failed to persist merged commit hash");
        let source_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == source_session_id)
            .expect("expected source session");
        source_session.status = crate::domain::session::Status::Done;
        source_session.title = Some("Done source".to_string());
        app.mode = AppMode::View {
            session_id: source_session_id.clone().into(),
            scroll_offset: Some(0),
        };
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        let event_result = handle_key_event(
            &mut app,
            &mut terminal,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::ContinueSession,
                ref confirmation_title,
                ref restore_view,
                ref session_id,
                ..
            } if confirmation_title == "Confirm Continue"
                && matches!(restore_view, Some(restore_view) if restore_view.session_id == source_session_id)
                && matches!(session_id, Some(session_id) if session_id.as_str() == source_session_id)
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_restores_view_for_merge_confirmation() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::MergeSession,
            confirmation_message: "Add this session to merge queue?".to_string(),
            confirmation_title: "Confirm Merge".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(6),
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref session_id_in_mode,
                scroll_offset: Some(6),
            } if session_id_in_mode == &session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_restores_view_for_continue_confirmation() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ContinueSession,
            confirmation_message: "Create a new draft session with initial context from this \
                                   session?"
                .to_string(),
            confirmation_title: "Confirm Continue".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 1,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref session_id_in_mode,
                scroll_offset: Some(4),
                ..
            } if session_id_in_mode == &session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_opens_continuation_draft_prompt() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
        let source_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.services
            .db()
            .sessions()
            .update_session_merged_commit_hash(
                &source_session_id,
                Some(merged_commit_hash.to_string()),
            )
            .await
            .expect("failed to persist merged commit hash");
        crate::test_support::persist_session_status_for_test(
            &mut app,
            &source_session_id,
            crate::domain::session::Status::Done,
        )
        .await;
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ContinueSession,
            confirmation_message: "Create a new draft session with initial context from this \
                                   session?"
                .to_string(),
            confirmation_title: "Confirm Continue".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: source_session_id.clone().into(),
            }),
            session_id: Some(source_session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref input,
                ref session_id,
                ..
            } if session_id.as_str() != source_session_id
                && input.text().is_empty()
        ));
        let continued_session_id = match &app.mode {
            AppMode::Prompt { session_id, .. } => session_id.as_str().to_string(),
            _ => unreachable!("expected prompt mode"),
        };
        let continued_session = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == continued_session_id)
            .expect("expected created continuation draft");
        assert_eq!(
            continued_session.prompt,
            format!("Use {merged_commit_hash} commit as an initial context for this session")
        );
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_queues_merge_with_view_restore() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::MergeSession,
            confirmation_message: "Add this session to merge queue?".to_string(),
            confirmation_title: "Confirm Merge".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(2),
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref session_id_in_mode,
                scroll_offset: Some(2),
            } if session_id_in_mode == &session_id
        ));
        app.sessions.sync_from_handles();
        let output = session_replay_text(&app.sessions.sessions()[0]);
        assert!(output.contains("[Merge Error]"));
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_escape_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(7),
                session_id: "session-id".into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(7),
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_enter_starts_pull_request_publish() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        crate::test_support::persist_session_status_for_test(
            &mut app,
            &session_id,
            crate::domain::session::Status::Review,
        )
        .await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::PublishPullRequest,
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: session_id.into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::ViewInfoPopup {
                is_loading: true,
                ref message,
                ref title,
                ..
            } if title == "Publishing review request"
                && message.contains("`review/custom`")
        ));
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_char_updates_input_state() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::default(),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::PublishBranchInput {
                input: ref input_state,
                ..
            } if input_state.cursor == 1 && input_state.text() == "r"
        ));
        let AppMode::PublishBranchInput { input, .. } = &app.mode else {
            unreachable!("mode should remain publish-branch input");
        };
        assert_eq!(input.text(), "r");
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_shortcut_chars_are_inserted() {
        // Arrange
        let typed_shortcut_characters = ['q', 'p', 'd', 'f', 'm', 'r', 'j', 'k', 'g', 'G', '?'];
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;

        for character in typed_shortcut_characters {
            app.mode = AppMode::PublishBranchInput {
                default_branch_name: "wt/session".to_string(),
                input: crate::domain::input::InputState::default(),
                locked_upstream_ref: None,
                publish_branch_action: crate::domain::session::PublishBranchAction::Push,
                restore_view: ConfirmationViewMode {
                    scroll_offset: None,
                    session_id: "session-id".into(),
                },
            };
            let modifiers = if character.is_ascii_uppercase() || character == '?' {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };

            // Act
            let event_result = handle_publish_branch_input_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), modifiers),
            );

            // Assert
            assert!(matches!(event_result, EventResult::Continue));
            assert!(matches!(
                app.mode,
                AppMode::PublishBranchInput {
                    input: ref input_state,
                    ..
                } if input_state.cursor == 1 && input_state.text() == character.to_string()
            ));
        }
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_left_moves_cursor() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        let AppMode::PublishBranchInput { input, .. } = &app.mode else {
            unreachable!("mode should remain publish-branch input");
        };
        assert_eq!(input.text(), "review/custom");
        assert_eq!(input.cursor, "review/custom".chars().count() - 1);
    }

    #[tokio::test]
    async fn test_handle_publish_branch_input_key_char_keeps_locked_branch_name() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::with_text("review/custom".to_string()),
            locked_upstream_ref: Some("origin/review/custom".to_string()),
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        let AppMode::PublishBranchInput {
            input,
            locked_upstream_ref,
            ..
        } = &app.mode
        else {
            unreachable!("mode should remain publish-branch input");
        };
        assert_eq!(locked_upstream_ref.as_deref(), Some("origin/review/custom"));
        assert_eq!(input.text(), "review/custom");
    }

    #[test]
    fn test_next_launch_configuration_index_wraps_to_start() {
        // Arrange
        let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];

        // Act
        let index = next_launch_configuration_index(1, &commands);

        // Assert
        assert_eq!(index, 0);
    }

    #[test]
    fn test_previous_launch_configuration_index_wraps_to_end() {
        // Arrange
        let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];

        // Act
        let index = previous_launch_configuration_index(0, &commands);

        // Assert
        assert_eq!(index, 1);
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_escape_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(3),
                session_id: "session-id".into(),
            },
            selected_command_index: 1,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(3),
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_j_updates_selected_index() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
            selected_command_index: 0,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::LaunchConfigurationSelector {
                selected_command_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_with_empty_commands_keeps_index_zero() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: Vec::new(),
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
            selected_command_index: 0,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::LaunchConfigurationSelector {
                selected_command_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_enter_restores_view_without_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: "session-id".into(),
            },
            selected_command_index: 0,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(4),
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_enter_runs_selected_command_in_tmux() {
        // Arrange
        let mut mock_tmux_client = MockTmuxClient::new();
        mock_tmux_client
            .expect_open_window_for_folder()
            .times(1)
            .returning(|_| Box::pin(async { Some("@24".to_string()) }));
        mock_tmux_client
            .expect_run_command_in_window()
            .with(eq("@24".to_string()), eq("npm run dev".to_string()))
            .times(1)
            .returning(|_, _| Box::pin(async {}));
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_tmux_client(Arc::new(mock_tmux_client))
                .await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(2),
                session_id: expected_session_id.clone().into(),
            },
            selected_command_index: 1,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(2),
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_launch_configuration_selector_key_unknown_key_preserves_state() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(1),
                session_id: "session-id".into(),
            },
            selected_command_index: 1,
        };

        // Act
        let event_result = handle_launch_configuration_selector_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::LaunchConfigurationSelector {
                selected_command_index: 1,
                ref commands,
                restore_view:
                    ConfirmationViewMode {
                        scroll_offset: Some(1),
                        ref session_id,
                    },
            } if commands == &vec!["cargo test".to_string(), "npm run dev".to_string()]
                && session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_cancel_restores_view_for_regenerate_confirmation() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::RegenerateReview,
            confirmation_message: "Regenerate focused review?".to_string(),
            confirmation_title: "Confirm Regenerate".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 1,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                scroll_offset: Some(4),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_confirmation_decision_confirm_regenerates_review() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_folder = app.sessions.sessions()[0].folder.clone();
        std::fs::write(session_folder.join("README.md"), "regenerate test\n")
            .expect("failed to write");
        app.review_cache.insert(
            session_id.clone().into(),
            ReviewCacheEntry::Ready {
                text: "Old review".to_string(),
                diff_hash: 99,
            },
        );
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::RegenerateReview,
            confirmation_message: "Regenerate focused review?".to_string(),
            confirmation_title: "Confirm Regenerate".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: None,
                session_id: session_id.clone().into(),
            }),
            session_id: Some(session_id.clone().into()),
            selected_confirmation_index: 0,
        };

        // Act
        let event_result =
            handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

        // Assert — view is restored with loading state, cache shows new Loading entry
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::View { .. }));
        assert!(matches!(
            app.review_cache.get(session_id.as_str()),
            Some(ReviewCacheEntry::Loading { .. })
        ));
    }
}
