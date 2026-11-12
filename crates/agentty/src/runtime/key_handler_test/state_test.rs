use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{handle_key_event, handle_launch_configuration_selector_key};
use super::support::FailingSizeBackend;
use crate::domain::input::InputState;
use crate::presentation::app_mode::{
    AppMode, ChatFocus, ConfirmationIntent, ConfirmationViewMode, DiffFocus, DiffLineCommentAnchor,
    DiffLineCommentTarget, DiffLineComments, DiffLineSide, DiffPreview, DiffRestoreTarget,
    DiffReviewComments, DiffSidebarFocus, HelpContext, PromptModeSnapshot,
};
use crate::presentation::execution_graph::tests::graph_view;
use crate::presentation::execution_graph::{GraphEditKind, GraphEditor};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};
use crate::runtime::{EventResult, PresentationState};

#[tokio::test]
async fn graph_keys_preserve_rejected_drafts_and_mark_the_view_for_redraw() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    let mut view = graph_view();
    view.editor = Some(GraphEditor {
        input: InputState::with_text("Unsent guidance".into()),
        kind: GraphEditKind::Reply,
        needs_reconciliation: true,
    });
    let draft = view.editor.clone();
    let progress = view.progress.clone();
    app.mode = AppMode::ExecutionGraph(Box::new(view));
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("terminal");
    let presentation = PresentationState::default();
    app.clear_redraw();

    // Act
    let result = handle_key_event(
        &mut app,
        &presentation,
        &mut terminal,
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(app.needs_redraw());
    assert!(matches!(&app.mode, AppMode::ExecutionGraph(view)
        if view.editor == draft && view.progress == progress && view.notice_scroll == 1));
}

#[tokio::test]
async fn test_handle_key_event_routes_prompt_history_and_restores_draft() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    let session = crate::test_support::SessionFixtureBuilder::new()
        .id("session-id")
        .folder(base_dir.path().to_path_buf())
        .build();
    app.sessions =
        crate::test_support::session_manager_with_handles(vec![session], HashMap::new()).into();
    app.mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::new(vec!["Earlier prompt".to_string()]),
        input: InputState::with_text("Unfinished draft".to_string()),
        scroll_offset: None,
        session_id: "session-id".into(),
        slash_state: PromptSlashState::default(),
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let presentation = PresentationState::default();
    app.clear_redraw();

    // Act
    let up_result = handle_key_event(
        &mut app,
        &presentation,
        &mut terminal,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(up_result, Ok(EventResult::Continue)));
    assert!(app.needs_redraw());
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { history_state, input, .. }
            if input.text() == "Earlier prompt"
                && history_state.selected_index == Some(0)
                && history_state.draft_text.as_deref() == Some("Unfinished draft")
    ));

    // Arrange
    app.clear_redraw();

    // Act
    let down_result = handle_key_event(
        &mut app,
        &presentation,
        &mut terminal,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(down_result, Ok(EventResult::Continue)));
    assert!(app.needs_redraw());
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { history_state, input, .. }
            if input.text() == "Unfinished draft"
                && history_state.selected_index.is_none()
                && history_state.draft_text.is_none()
                && history_state.draft_input_revision.is_none()
    ));
}

#[tokio::test]
async fn test_handle_key_event_routes_help_dismissal_and_requests_redraw() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Help {
        context: HelpContext::List {
            keybindings: Vec::new(),
        },
        scroll_offset: 3,
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    app.clear_redraw();

    // Act
    let result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::List));
    assert!(app.needs_redraw());
}

/// Builds one overlay that restores the `session-id` view when closed.
fn view_info_popup(is_loading: bool) -> AppMode {
    AppMode::ViewInfoPopup {
        is_loading,
        loading_label: "Refreshing review request...".to_string(),
        message: "Review request refreshed.".to_string(),
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(2),
            session_id: "session-id".into(),
        },
        title: "Review request refreshed".to_string(),
    }
}

/// Builds one launch-configuration selector over `commands`.
fn launch_configuration_selector(commands: &[&str], selected_command_index: usize) -> AppMode {
    AppMode::LaunchConfigurationSelector {
        commands: commands.iter().map(ToString::to_string).collect(),
        restore_view: ConfirmationViewMode {
            scroll_offset: None,
            session_id: "session-id".into(),
        },
        selected_command_index,
    }
}

/// One dispatcher case: starting mode, pressed key, and expected outcome.
type DispatchCase = (AppMode, KeyCode, fn(&AppMode) -> bool);

/// Returns dispatcher cases for the session-creation and project selectors.
fn selector_dispatch_cases() -> Vec<DispatchCase> {
    vec![
        (
            AppMode::SessionCreation {
                selected_option_index: 1,
            },
            KeyCode::Char('Q'),
            |mode| matches!(mode, AppMode::List),
        ),
        (
            AppMode::SessionCreation {
                selected_option_index: 1,
            },
            KeyCode::Char('x'),
            |mode| {
                matches!(
                    mode,
                    AppMode::SessionCreation {
                        selected_option_index: 1
                    }
                )
            },
        ),
        (
            AppMode::ProjectSwitcher {
                selected_option_index: 0,
            },
            KeyCode::Char('Q'),
            |mode| matches!(mode, AppMode::List),
        ),
        (
            AppMode::ProjectSwitcher {
                selected_option_index: 0,
            },
            KeyCode::Char('x'),
            |mode| {
                matches!(
                    mode,
                    AppMode::ProjectSwitcher {
                        selected_option_index: 0
                    }
                )
            },
        ),
        (
            AppMode::ProjectSwitcher {
                selected_option_index: 9,
            },
            KeyCode::Enter,
            |mode| matches!(mode, AppMode::List),
        ),
    ]
}

/// Returns dispatcher cases for informational popups and warnings.
fn popup_dispatch_cases() -> Vec<DispatchCase> {
    vec![
        (
            AppMode::PreCommitHookWarning {
                message: "Install hooks".to_string(),
            },
            KeyCode::Char('Q'),
            |mode| matches!(mode, AppMode::List),
        ),
        (
            AppMode::PreCommitHookWarning {
                message: "Install hooks".to_string(),
            },
            KeyCode::Char('x'),
            |mode| matches!(mode, AppMode::PreCommitHookWarning { .. }),
        ),
        (
            AppMode::SyncBlockedPopup {
                default_branch: None,
                is_loading: false,
                message: "Main is dirty".to_string(),
                project_name: None,
                title: "Sync blocked".to_string(),
            },
            KeyCode::Esc,
            |mode| matches!(mode, AppMode::List),
        ),
        (view_info_popup(true), KeyCode::Esc, |mode| {
            matches!(
                mode,
                AppMode::ViewInfoPopup {
                    is_loading: true,
                    ..
                }
            )
        }),
        (view_info_popup(false), KeyCode::Char('Q'), |mode| {
            matches!(
                mode,
                AppMode::View {
                    scroll_offset: Some(2),
                    session_id,
                } if session_id == "session-id"
            )
        }),
        (view_info_popup(false), KeyCode::Char('x'), |mode| {
            matches!(mode, AppMode::ViewInfoPopup { .. })
        }),
    ]
}

/// Returns dispatcher cases for launch selection, diff loading, and
/// confirmation overlays.
fn transient_overlay_dispatch_cases() -> Vec<DispatchCase> {
    vec![
        (
            launch_configuration_selector(&["cargo test", "npm run dev"], 0),
            KeyCode::Char('k'),
            |mode| {
                matches!(
                    mode,
                    AppMode::LaunchConfigurationSelector {
                        selected_command_index: 1,
                        ..
                    }
                )
            },
        ),
        (
            launch_configuration_selector(&["cargo test", "npm run dev"], 1),
            KeyCode::Up,
            |mode| {
                matches!(
                    mode,
                    AppMode::LaunchConfigurationSelector {
                        selected_command_index: 0,
                        ..
                    }
                )
            },
        ),
        (
            launch_configuration_selector(&[], 0),
            KeyCode::Char('k'),
            |mode| {
                matches!(
                    mode,
                    AppMode::LaunchConfigurationSelector {
                        selected_command_index: 0,
                        ..
                    }
                )
            },
        ),
        (
            AppMode::DiffLoading {
                fallback_view_scroll_offset: None,
                request_id: 1,
                restore: None,
                session_id: "session-id".into(),
                sidebar_focus: DiffSidebarFocus::Files,
            },
            KeyCode::Char('x'),
            |mode| matches!(mode, AppMode::DiffLoading { request_id: 1, .. }),
        ),
        (
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::Quit,
                confirmation_message: "Quit?".to_string(),
                confirmation_title: "Confirm Quit".to_string(),
                restore_view: None,
                session_id: None,
                selected_confirmation_index: 1,
            },
            KeyCode::Char('x'),
            |mode| {
                matches!(
                    mode,
                    AppMode::Confirmation {
                        selected_confirmation_index: 1,
                        ..
                    }
                )
            },
        ),
    ]
}

#[tokio::test]
async fn overlay_keys_route_through_the_dispatcher_and_request_redraw() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let presentation = PresentationState::default();
    let cases = selector_dispatch_cases()
        .into_iter()
        .chain(popup_dispatch_cases())
        .chain(transient_overlay_dispatch_cases());

    for (initial_mode, key_code, expected_mode) in cases {
        app.mode = initial_mode;
        app.clear_redraw();

        // Act
        let result = handle_key_event(
            &mut app,
            &presentation,
            &mut terminal,
            KeyEvent::new(key_code, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert!(
            expected_mode(&app.mode),
            "unexpected mode after {key_code:?}"
        );
        assert!(app.needs_redraw());
    }
}

#[tokio::test]
async fn failed_terminal_queries_surface_without_requesting_redraw() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        questions: Vec::new(),
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: "session-id".into(),
    };
    let mut terminal =
        ratatui::Terminal::new(FailingSizeBackend::new(80, 24)).expect("failed to create terminal");
    terminal.backend().fail_size_queries();
    app.clear_redraw();

    // Act
    let result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(ref error) if error.to_string() == "terminal size unavailable"
    ));
    assert!(!app.needs_redraw());
}

#[tokio::test]
async fn test_handle_key_event_routes_chatgpt_sign_in_cancel_to_the_composer() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = crate::test_support::chatgpt_sign_in_mode("session-id");
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "draft"
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
async fn test_handle_key_event_routes_done_session_continue_shortcut() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
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
        &PresentationState::default(),
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
async fn test_handle_key_event_routes_loading_diff_cancel() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::DiffLoading {
        fallback_view_scroll_offset: Some(3),
        request_id: 1,
        restore: None,
        session_id: "session-id".into(),
        sidebar_focus: DiffSidebarFocus::Files,
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let event_result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
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
async fn test_handle_key_event_submits_completed_diff_comments() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    let session = crate::test_support::SessionFixtureBuilder::new()
        .id("session-id")
        .folder(base_dir.path().to_path_buf())
        .status(crate::domain::session::Status::Review)
        .build();
    app.sessions =
        crate::test_support::session_manager_with_handles(vec![session], HashMap::new()).into();
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "review();".to_string(),
        line: 1,
        path: "src/main.rs".to_string(),
        side: DiffLineSide::New,
    }));
    line_comments
        .editing_input_mut()
        .expect("seeded comment should be editable")
        .insert_text("Explain this call");
    line_comments.finish_editing();
    app.mode = AppMode::Diff {
        diff: "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
        file_explorer_selected_index: 1,
        focus: DiffFocus::Files,
        line_comments,
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(DiffReviewComments {
            sidebar_focus: DiffSidebarFocus::Comments,
            ..DiffReviewComments::loading(1)
        }),
        restore: Some(Box::new(DiffRestoreTarget::Prompt(PromptModeSnapshot {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::default(),
            input: crate::domain::input::InputState::with_text("/keep draft".to_string()),
            scroll_offset: None,
            session_id: "session-id".into(),
            slash_state: PromptSlashState::default(),
        }))),
        scroll_cache: None,
        session_id: "session-id".into(),
        scroll_offset: 0,
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let event_result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::View { .. }));

    // Arrange
    app.mode = AppMode::Diff {
        diff: "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
        file_explorer_selected_index: 1,
        focus: DiffFocus::Content,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        session_id: "session-id".into(),
        scroll_offset: 0,
    };
    app.clear_redraw();
    assert!(!app.needs_redraw());

    // Act
    let no_submit_result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(no_submit_result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::Diff { .. }));
    assert!(app.needs_redraw());
}
