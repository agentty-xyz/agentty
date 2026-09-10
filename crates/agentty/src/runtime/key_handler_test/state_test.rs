use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{handle_key_event, handle_launch_configuration_selector_key};
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, ConfirmationViewMode, DiffFocus, DiffLineCommentAnchor,
    DiffLineCommentTarget, DiffLineComments, DiffLineSide, DiffPreview, DiffRestoreTarget,
    DiffReviewComments, DiffSidebarFocus, PromptModeSnapshot,
};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};
use crate::runtime::{EventResult, PresentationState};

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
