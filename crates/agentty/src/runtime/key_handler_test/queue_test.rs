use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::handle_key_event;
use crate::domain::session::SessionHandles;
use crate::presentation::app_mode::{
    AppMode, DiffCommentTarget, DiffFocus, DiffLineComments, DiffPreview, DiffSidebarFocus,
};
use crate::runtime::{PresentationState, mode};

#[tokio::test]
async fn test_diff_comment_queue_cancel_restores_comments() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &session_id,
        crate::domain::session::Status::InProgress,
    );
    app.sessions.session_handles_mut().insert(
        session_id.clone().into(),
        SessionHandles::new(crate::domain::session::Status::InProgress),
    );
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
    line_comments
        .editing_input_mut()
        .expect("file comment should be editable")
        .insert_text("Keep this queued comment");
    line_comments.finish_editing();
    app.mode = AppMode::Diff {
        diff: "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session_id: session_id.clone().into(),
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act — submit from Diff while another turn runs, then retract the
    // queued prompt.
    handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    )
    .await
    .expect("diff comment submission should succeed");
    handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await
    .expect("queued comment cancellation should succeed");
    mode::diff::tests::support::enter_diff_mode(
        &mut app,
        &session_id,
        "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
        None,
        DiffSidebarFocus::Files,
    );

    // Assert
    let queued_messages = app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("session handles should remain available")
        .queued_messages
        .lock()
        .expect("queued message lock should remain available");
    assert!(queued_messages.is_empty());
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.prompt_text().contains("Keep this queued comment")
    ));
    assert!(!app.diff_comment_progress.contains_key(session_id.as_str()));
}
