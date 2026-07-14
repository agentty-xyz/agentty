use std::io;

use ag_tui_text::text_util::inline_text;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Tab};
use crate::domain::input::InputState;
use crate::domain::session::{Session, Status};
use crate::presentation::app_mode::{AppMode, ChatFocus, ConfirmationIntent, HelpContext};
use crate::presentation::help_action::{
    HelpAction, project_list_actions, session_list_actions, settings_actions,
};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState};
use crate::runtime::EventResult;
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;

/// Handles key input while the app is in list mode.
///
/// Pressing `q` opens a confirmation overlay instead of quitting immediately,
/// with `No` selected by default. Pressing `Enter` on the `Projects` tab
/// selects the active project and then moves focus to `Tab::Sessions`.
/// `c` opens a cancel confirmation overlay for running sessions, review
/// sessions, and unstarted draft sessions, and `Tab` cycles tabs forward while
/// `Shift+Tab` cycles backward.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    if app.tabs.current() == Tab::Settings
        && app.settings.is_launch_configuration_list_editor_open()
    {
        return handle_settings_launch_configuration_list_editor(app, key).await;
    }

    if app.tabs.current() == Tab::Settings && app.settings.is_selector_dropdown_open() {
        return handle_settings_selector_dropdown(app, key).await;
    }

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::Quit,
                confirmation_message: "Quit agentty?".to_string(),
                confirmation_title: "Confirm Quit".to_string(),
                restore_view: None,
                session_id: None,
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            };

            return Ok(EventResult::Continue);
        }
        KeyCode::Tab => {
            app.next_tab();
            app.persist_current_tab().await;
        }
        KeyCode::BackTab => {
            app.previous_tab();
            app.persist_current_tab().await;
        }
        KeyCode::Char('a')
            if app.tabs.current() == Tab::Sessions && key.modifiers == KeyModifiers::NONE =>
        {
            app.mode = AppMode::SessionCreation {
                selected_option_index: 0,
            };
        }
        KeyCode::Char('p')
            if app.tabs.current() == Tab::Sessions && key.modifiers == KeyModifiers::NONE =>
        {
            app.mode = AppMode::ProjectSwitcher {
                selected_option_index: 0,
            };
        }
        KeyCode::Char('j') | KeyCode::Down => match app.tabs.current() {
            Tab::Projects => app.next_project(),
            Tab::Sessions => app.next(),
            Tab::Review => app.next_requested_review(),
            Tab::Issues => app.next_assigned_issue(),
            Tab::Settings => app.settings.next(),
        },
        KeyCode::Char('k') | KeyCode::Up => match app.tabs.current() {
            Tab::Projects => app.previous_project(),
            Tab::Sessions => app.previous(),
            Tab::Review => app.previous_requested_review(),
            Tab::Issues => app.previous_assigned_issue(),
            Tab::Settings => app.settings.previous(),
        },
        KeyCode::Enter => return handle_enter_key(app).await,
        KeyCode::Char('c') if app.tabs.current() == Tab::Sessions => {
            let selected_session = app.selected_session().and_then(|session| {
                session
                    .allows_cancel_action()
                    .then(|| (session.id.clone(), inline_text(session.display_title())))
            });
            if let Some((session_id, session_title)) = selected_session {
                app.mode = AppMode::Confirmation {
                    confirmation_intent: ConfirmationIntent::CancelSession,
                    confirmation_message: format!("Cancel session \"{session_title}\"?"),
                    confirmation_title: "Confirm Cancel".to_string(),
                    restore_view: None,
                    session_id: Some(session_id),
                    selected_confirmation_index: DEFAULT_OPTION_INDEX,
                };
            }
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'s') => sync_list_context(app),
        KeyCode::Char('?') => {
            open_list_help_overlay(app);
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Handles `Enter` in list mode and triggers the selected tab primary action.
///
/// On the sessions tab, any selected session can be opened in view mode.
async fn handle_enter_key(app: &mut App) -> io::Result<EventResult> {
    match app.tabs.current() {
        Tab::Projects => {
            if app.switch_selected_project().await.is_ok() {
                app.tabs.set(Tab::Sessions);
                app.persist_current_tab().await;
            }
        }
        Tab::Sessions => {
            if let Some(session_index) = app.sessions.selected_session_index() {
                let Some(session_id) = app
                    .sessions
                    .session_at(session_index)
                    .map(|session| session.id.clone())
                else {
                    return Ok(EventResult::Continue);
                };

                app.sessions
                    .load_session_detail_into_state(app.services.db(), session_id.as_str())
                    .await;

                if let Some(session) = app.sessions.session_at(session_index)
                    && session.status == Status::Question
                {
                    let questions = session.questions.clone();
                    app.enter_question_mode(session_id.as_str(), questions);
                } else {
                    app.mode = AppMode::View {
                        session_id,
                        scroll_offset: None,
                    };
                }
            }
        }
        Tab::Settings => {
            app.settings.handle_enter();
        }
        Tab::Review => {
            app.open_selected_requested_review();
        }
        Tab::Issues => {
            app.open_selected_assigned_issue();
        }
    }

    Ok(EventResult::Continue)
}

/// Handles key input while a settings selector dropdown is open.
async fn handle_settings_selector_dropdown(
    app: &mut App,
    key: KeyEvent,
) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Esc => {
            app.settings.close_selector_dropdown();
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.settings.close_selector_dropdown();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.settings.next_selector_dropdown_option();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.settings.previous_selector_dropdown_option();
        }
        KeyCode::Enter => {
            app.settings
                .select_selector_dropdown_option(&app.services)
                .await;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Handles key input while the `Launch Configurations` list editor is open.
async fn handle_settings_launch_configuration_list_editor(
    app: &mut App,
    key: KeyEvent,
) -> io::Result<EventResult> {
    if app
        .settings
        .is_launch_configuration_list_editor_input_active()
    {
        return handle_settings_launch_configuration_input(app, key).await;
    }

    match key.code {
        KeyCode::Esc => {
            app.settings.close_launch_configuration_list_editor();
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => {
            app.settings.close_launch_configuration_list_editor();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.settings.next_launch_configuration_list_editor_item();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.settings
                .previous_launch_configuration_list_editor_item();
        }
        KeyCode::Char('J') => {
            app.settings
                .move_selected_launch_configuration_down(&app.services)
                .await;
        }
        KeyCode::Char('K') => {
            app.settings
                .move_selected_launch_configuration_up(&app.services)
                .await;
        }
        KeyCode::Char('a') => {
            app.settings.start_adding_launch_configuration();
        }
        KeyCode::Char('e') | KeyCode::Enter => {
            app.settings.start_editing_selected_launch_configuration();
        }
        KeyCode::Char('d') => {
            app.settings
                .delete_selected_launch_configuration(&app.services)
                .await;
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Handles key input while adding or editing one launch configuration.
async fn handle_settings_launch_configuration_input(
    app: &mut App,
    key: KeyEvent,
) -> io::Result<EventResult> {
    match key.code {
        KeyCode::Enter => {
            app.settings
                .confirm_launch_configuration_input(&app.services)
                .await;
        }
        KeyCode::Esc => {
            app.settings.cancel_launch_configuration_input();
        }
        KeyCode::Left => {
            app.settings.move_launch_configuration_input_cursor_left();
        }
        KeyCode::Right => {
            app.settings.move_launch_configuration_input_cursor_right();
        }
        KeyCode::Home => {
            app.settings.move_launch_configuration_input_cursor_home();
        }
        KeyCode::End => {
            app.settings.move_launch_configuration_input_cursor_end();
        }
        KeyCode::Backspace => {
            app.settings.remove_launch_configuration_input_character();
        }
        KeyCode::Delete => {
            app.settings.delete_launch_configuration_input_character();
        }
        KeyCode::Char(character) if is_settings_text_key(key) => {
            app.settings
                .append_launch_configuration_input_character(character);
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Returns whether a key event should insert text into a settings string value.
fn is_settings_text_key(key: KeyEvent) -> bool {
    key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT
}

/// Starts the sync action that applies to the visible list tab.
fn sync_list_context(app: &mut App) {
    if app.tabs.current() == Tab::Issues {
        app.refresh_assigned_issues();

        return;
    }

    if app.tabs.current() == Tab::Review {
        app.refresh_requested_reviews_for_current_project();

        return;
    }

    app.start_sync_main();
}

/// Opens the help overlay with list-mode action availability projection.
fn open_list_help_overlay(app: &mut App) {
    let keybindings = list_keybindings(app);

    app.mode = AppMode::Help {
        context: HelpContext::List { keybindings },
        scroll_offset: 0,
    };
}

/// Projects current list-mode action availability into keybinding entries.
fn list_keybindings(app: &App) -> Vec<HelpAction> {
    if app.tabs.current() == Tab::Projects {
        return project_list_actions();
    }

    if app.tabs.current() == Tab::Settings {
        return settings_actions();
    }

    if app.tabs.current() == Tab::Review {
        return crate::presentation::help_action::review_actions();
    }

    if app.tabs.current() == Tab::Issues {
        return crate::presentation::help_action::issue_actions();
    }

    let is_sessions_tab = app.tabs.current() == Tab::Sessions;
    let selected_session = app.selected_session();
    let can_cancel_selected_session =
        is_sessions_tab && selected_session.is_some_and(Session::allows_cancel_action);
    let can_open_selected_session = is_sessions_tab
        && app
            .sessions
            .selected_session_index()
            .and_then(|selected_index| app.sessions.session_at(selected_index))
            .is_some();
    session_list_actions(can_cancel_selected_session, can_open_selected_session)
}

/// Opens prompt mode for the provided session identifier.
pub(crate) fn open_session_prompt(app: &mut App, session_id: String) {
    app.mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::new(Vec::new()),
        slash_state: app.prompt_slash_state(),
        session_id: session_id.into(),
        input: InputState::default(),
        scroll_offset: None,
    };
}

#[cfg(test)]
mod tests {
    use ag_forge::{ForgeKind, RequestedReview, RequestedReviewAudience};
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::app::{AppEvent, MockSyncMainRunner, SyncMainOutcome, SyncSessionStartError};
    use crate::domain::question::QuestionItem;
    use crate::domain::theme::ColorTheme;

    /// Builds a settings-focused test app with the `Launch Configurations` row
    /// selected.
    async fn new_test_app_for_settings() -> (App, tempfile::TempDir) {
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        app.create_session()
            .await
            .expect("failed to create session for settings tests");
        app.tabs.set(Tab::Settings);
        let launch_configuration_row_index = app
            .settings
            .settings_rows()
            .iter()
            .position(|(setting_name, _)| *setting_name == "Launch Configurations")
            .expect("missing Launch Configurations setting row");
        app.settings
            .table_state
            .select(Some(launch_configuration_row_index));

        (app, base_dir)
    }

    /// Replaces sync background execution with one immediate completion event.
    fn mock_sync_main_completion(
        app: &mut App,
        result: Result<SyncMainOutcome, SyncSessionStartError>,
    ) {
        let mut mock_sync_main_runner = MockSyncMainRunner::new();
        mock_sync_main_runner
            .expect_start_sync_main()
            .times(1)
            .returning(move |app_event_tx, _, _, _, _| {
                let _ = app_event_tx.send(AppEvent::SyncMainCompleted {
                    result: result.clone(),
                });
            });
        app.sync_main_runner = std::sync::Arc::new(mock_sync_main_runner);
    }

    #[tokio::test]
    async fn test_handle_quit_key_shows_confirm_quit_overlay() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::Quit,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: None,
                session_id: None,
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if confirmation_title == "Confirm Quit" && confirmation_message == "Quit agentty?"
        ));
    }

    #[tokio::test]
    async fn test_handle_backtab_key_cycles_tabs_backward() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Projects);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.tabs.current(), Tab::Settings);
    }

    #[tokio::test]
    async fn test_handle_add_key_opens_session_creation_overlay() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Sessions);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(
            app.mode,
            AppMode::SessionCreation {
                selected_option_index: 0,
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_project_switcher_key_opens_switcher_on_sessions_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Sessions);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::ProjectSwitcher {
                selected_option_index: 0,
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_project_switcher_key_ignored_on_projects_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Projects);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_add_key_ignored_on_projects_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Projects);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_add_key_ignored_on_settings_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Settings);

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_selected_session_in_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_restores_cached_review_output() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.review_cache.insert(
            expected_session_id.clone().into(),
            crate::app::ReviewCacheEntry::Ready {
                diff_hash: 7,
                text: "Focused review".to_string(),
            },
        );
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_selected_question_session_in_question_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let expected_questions: Vec<QuestionItem> = vec![
            QuestionItem {
                options: vec!["main".to_string(), "develop".to_string()],
                text: "Need a target branch?".to_string(),
            },
            QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Need migration notes?".to_string(),
            },
        ];
        if let Some(session) = app.sessions.sessions_mut().first_mut() {
            session.status = Status::Question;
            session.questions = expected_questions.clone();
        }
        app.review_cache.insert(
            expected_session_id.clone().into(),
            crate::app::ReviewCacheEntry::Ready {
                text: "Focused review".to_string(),
                diff_hash: 42,
            },
        );
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                ref questions,
                current_index: 0,
                ref responses,
                ref input,
                selected_option_index: Some(0),
                ..
            } if session_id == &expected_session_id
                && questions == &expected_questions
                && responses.is_empty()
                && input.text().is_empty()
        ));
    }

    #[tokio::test]
    async fn test_inbox_tab_navigation_selects_loaded_reviews() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Review);
        app.replace_requested_reviews(
            app.projects.active_project_id(),
            vec![
                requested_review("First review body"),
                requested_review("Second body"),
            ],
        );

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.requested_review_selected_index(), Some(1));
    }

    #[tokio::test]
    async fn test_inbox_tab_enter_opens_selected_review_detail() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Review);
        app.replace_requested_reviews(
            app.projects.active_project_id(),
            vec![
                requested_review("First review body"),
                requested_review("Second body"),
            ],
        );
        app.next_requested_review();

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::ReviewDetail {
                ref review,
                scroll_offset,
                ..
            } if review.body.as_deref() == Some("Second body")
                && scroll_offset == 0
        ));
    }

    #[tokio::test]
    async fn test_inbox_tab_enter_uses_grouped_selection_order_after_mixed_provider_order() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Review);
        app.replace_requested_reviews(
            app.projects.active_project_id(),
            vec![
                requested_review_with_audience(RequestedReviewAudience::Group, "Group body"),
                requested_review_with_audience(RequestedReviewAudience::Personal, "Personal body"),
            ],
        );

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::ReviewDetail {
                ref review,
                scroll_offset,
                ..
            } if review.audience == RequestedReviewAudience::Personal
                && review.body.as_deref() == Some("Personal body")
                && scroll_offset == 0
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_keeps_persisted_size_until_turn_completion() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.services
            .db()
            .sessions()
            .update_session_diff_stats(0, 0, &expected_session_id, "XS")
            .await
            .expect("failed to set stale size");
        let session_index = app
            .session_index_for_id(&expected_session_id)
            .expect("missing created session");
        let session_folder = app.sessions.sessions()[session_index].folder.clone();
        let changed_lines = "line\n".repeat(40);
        std::fs::write(session_folder.join("open-size-test.txt"), changed_lines)
            .expect("failed to write test file");
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");
        let db_sessions = app
            .services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        let db_size = db_sessions
            .iter()
            .find(|db_session| db_session.id == expected_session_id)
            .map(|db_session| db_session.size.clone())
            .expect("missing persisted session");
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == &expected_session_id
        ));
        assert_eq!(db_size, "XS");
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_done_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        if let Some(session) = app.sessions.sessions_mut().first_mut() {
            session.status = Status::Done;
        }
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_canceled_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        if let Some(session) = app.sessions.sessions_mut().first_mut() {
            session.status = Status::Canceled;
        }
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_key_switches_to_sessions_tab_from_projects_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Projects);
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.tabs.current(), Tab::Sessions);
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_launch_configuration_list_editor_in_settings_tab() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");
        let editor = app
            .settings
            .launch_configuration_list_editor()
            .expect("expected launch-configuration list editor");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(
            editor.commands,
            vec!["cargo test".to_string(), "npm run dev".to_string()]
        );
    }

    #[tokio::test]
    async fn test_handle_enter_key_opens_settings_selector_dropdown() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Settings);
        app.settings.table_state.select(Some(0));

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");
        let selector_dropdown = app
            .settings
            .selector_dropdown()
            .expect("expected settings selector dropdown");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(selector_dropdown.row_index, 0);
        assert_eq!(selector_dropdown.selected_index, 0);
        assert_eq!(selector_dropdown.options[0].label, "Agentty Default");
    }

    #[tokio::test]
    async fn test_settings_selector_dropdown_keys_select_theme_value() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Settings);
        app.settings.table_state.select(Some(0));
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings selector dropdown");

        // Act
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to move dropdown selection");
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to select dropdown option");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.theme, ColorTheme::Green);
        assert!(!app.settings.is_selector_dropdown_open());
    }

    #[tokio::test]
    async fn test_settings_selector_dropdown_q_closes_without_quit_confirmation() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.tabs.set(Tab::Settings);
        app.settings.table_state.select(Some(0));
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings selector dropdown");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to close settings selector dropdown");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(!app.settings.is_selector_dropdown_open());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_adds_command_on_confirm() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to start adding launch configuration");

        // Act
        for character in "nvim".chars() {
            handle(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            )
            .await
            .expect("failed to type launch configuration");
        }
        assert_eq!(app.settings.launch_configuration, "");

        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to confirm launch configuration");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.launch_configuration, "nvim");
        assert_eq!(app.sessions.sessions().len(), 1);
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_backspace_updates_pending_input_only() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to start adding launch configuration");
        for character in "abc".chars() {
            handle(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            )
            .await
            .expect("failed to type launch configuration");
        }

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");
        let editor = app
            .settings
            .launch_configuration_list_editor()
            .expect("expected launch-configuration list editor");
        let input = editor.input.expect("expected active input");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(input.text(), "ab");
        assert_eq!(app.settings.launch_configuration, "");
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_edits_selected_command() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to select second command");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to start editing launch configuration");
        for _ in 0.."npm run dev".chars().count() {
            handle(
                &mut app,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            )
            .await
            .expect("failed to clear command");
        }

        // Act
        for character in "lazygit".chars() {
            handle(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            )
            .await
            .expect("failed to type replacement command");
        }
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to confirm edited launch configuration");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.launch_configuration, "cargo test\nlazygit");
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_delete_removes_selected_command() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to select second command");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.launch_configuration, "cargo test");
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_reorders_selected_command() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to reorder launch configuration");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.settings.launch_configuration, "npm run dev\ncargo test");
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_esc_closes_from_browse() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle Esc key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(!app.settings.is_launch_configuration_list_editor_open());
    }

    #[tokio::test]
    async fn test_launch_configuration_list_editor_esc_cancels_input_without_closing() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_for_settings().await;
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to open settings command editor");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to start adding launch configuration");
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to type launch configuration");

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle Esc key");
        let editor = app
            .settings
            .launch_configuration_list_editor()
            .expect("expected launch-configuration list editor");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.settings.is_launch_configuration_list_editor_open());
        assert!(editor.input.is_none());
        assert_eq!(app.settings.launch_configuration, "");
    }

    #[tokio::test]
    async fn test_handle_enter_key_without_session_selection_keeps_list_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        app.tabs.set(Tab::Sessions);
        app.mode = AppMode::List;

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_cancel_key_opens_cancel_confirmation_for_review_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions.sessions_mut()[0].status = Status::Review;
        let expected_session_title = app.sessions.sessions()[0].display_title().to_string();
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::CancelSession,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: None,
                session_id: Some(ref mode_session_id),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if mode_session_id == &expected_session_id
                && confirmation_title == "Confirm Cancel"
                && confirmation_message == &format!("Cancel session \"{expected_session_title}\"?")
        ));
    }

    #[tokio::test]
    async fn test_handle_cancel_key_opens_cancel_confirmation_for_draft_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_draft_session()
            .await
            .expect("failed to create draft session");
        let expected_session_title = app.sessions.sessions()[0].display_title().to_string();
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::CancelSession,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: None,
                session_id: Some(ref mode_session_id),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if mode_session_id == &expected_session_id
                && confirmation_title == "Confirm Cancel"
                && confirmation_message == &format!("Cancel session \"{expected_session_title}\"?")
        ));
    }

    #[tokio::test]
    async fn test_handle_cancel_key_opens_cancel_confirmation_for_running_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        let expected_session_title = app.sessions.sessions()[0].display_title().to_string();
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::CancelSession,
                ref confirmation_message,
                ref confirmation_title,
                restore_view: None,
                session_id: Some(ref mode_session_id),
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            } if mode_session_id == &expected_session_id
                && confirmation_title == "Confirm Cancel"
                && confirmation_message == &format!("Cancel session \"{expected_session_title}\"?")
        ));
    }

    #[tokio::test]
    async fn test_handle_cancel_key_ignores_non_review_session() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        let _session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        app.tabs.set(Tab::Sessions);
        app.sessions.select_session_index(Some(0));

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_question_mark_opens_help_overlay() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::List { ref keybindings },
                scroll_offset: 0,
            } if keybindings.iter().any(|action| action.key == "q")
                && keybindings.iter().any(|action| action.key == "?")
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_shows_failure_when_upstream_is_missing() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        mock_sync_main_completion(
            &mut app,
            Err(SyncSessionStartError::Other("missing upstream".to_string())),
        );

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ref title,
                ..
            } if title == "Sync in progress"
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: false,
                ref title,
                ..
            } if title == "Sync failed"
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_is_case_insensitive() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        mock_sync_main_completion(
            &mut app,
            Err(SyncSessionStartError::Other("missing upstream".to_string())),
        );

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ref title,
                ..
            } if title == "Sync in progress"
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: false,
                ref title,
                ..
            } if title == "Sync failed"
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_uses_project_name_and_branch_in_popup_message() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        mock_sync_main_completion(
            &mut app,
            Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: "develop".to_string(),
            }),
        );
        let expected_project_name = base_dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("expected temp dir file name")
            .to_string();
        app.projects.update_active_project_context(
            app.active_project_id(),
            app.projects.project_name().to_string(),
            Some("develop".to_string()),
            None,
            base_dir.path().to_path_buf(),
        );

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ref title,
                ..
            } if title == "Sync in progress"
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                ref default_branch,
                is_loading: false,
                ref title,
                ref message,
                ref project_name,
            } if title == "Sync blocked"
                && default_branch.as_deref() == Some("develop")
                && message.contains("cannot run while `develop` has uncommitted changes")
                && project_name.as_deref() == Some(expected_project_name.as_str())
        ));
    }

    #[tokio::test]
    async fn test_handle_sync_key_opens_popup_when_main_has_uncommitted_changes() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
        mock_sync_main_completion(
            &mut app,
            Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: "main".to_string(),
            }),
        );

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ref title,
                ..
            } if title == "Sync in progress"
        ));

        // Act
        app.process_pending_app_events().await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                ref default_branch,
                is_loading: false,
                ref title,
                ref message,
                ref project_name,
            } if title == "Sync blocked" && message.contains("cannot run while `main` has uncommitted changes")
                && default_branch.as_deref() == Some("main")
                && project_name.is_some()
        ));
    }

    /// Builds one requested review fixture for review-list navigation tests.
    fn requested_review(body: &str) -> RequestedReview {
        requested_review_with_audience(RequestedReviewAudience::Personal, body)
    }

    /// Builds one requested review fixture with the requested audience.
    fn requested_review_with_audience(
        audience: RequestedReviewAudience,
        body: &str,
    ) -> RequestedReview {
        RequestedReview {
            audience,
            author: "octocat".to_string(),
            body: Some(body.to_string()),
            comment_snapshot: None,
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            repository: "agentty-xyz/agentty".to_string(),
            status_summary: None,
            title: "Add review detail page".to_string(),
            updated_at: Some("2026-04-27T21:30:00Z".to_string()),
            web_url: "https://example.com/42".to_string(),
        }
    }
}
