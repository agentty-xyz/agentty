use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::open_line_comment_prompt;
use super::support::{TEST_TERMINAL_SIZE, diff_mode_fixture, handle, preview_test_app};
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineCommentAnchor, DiffLineCommentTarget, DiffPreview, HelpContext,
};

#[tokio::test]
async fn test_merged_diff_rejects_inline_comment_creation_and_submission() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Merged;
    let diff = "diff --git a/src/main.rs b/src/main.rs\n+review();\n";
    app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());

    // Act — whole-file comments are unavailable in read-only diffs.
    if let AppMode::Diff { focus, .. } = &mut app.mode {
        *focus = DiffFocus::Files;
    }
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
    );
    if let AppMode::Diff { focus, .. } = &mut app.mode {
        *focus = DiffFocus::Content;
    }

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.comments.is_empty() && !line_comments.is_selecting()
    ));

    // Arrange
    if let AppMode::Diff { line_comments, .. } = &mut app.mode {
        line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
            content: "review();".to_string(),
            line: 1,
            path: "src/main.rs".to_string(),
            side: crate::presentation::app_mode::DiffLineSide::New,
        }));
        line_comments
            .editing_input_mut()
            .expect("seeded inline comment should be editable")
            .insert_text("read-only comment");
        line_comments.finish_editing();
    }

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    );
    open_line_comment_prompt(&mut app);
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Help {
            context: HelpContext::Diff {
                can_comment: false,
                ..
            },
            ..
        }
    ));
}
