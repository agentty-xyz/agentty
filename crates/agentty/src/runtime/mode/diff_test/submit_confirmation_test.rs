use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{is_submit_confirmation_open, should_submit_line_comments};
use super::support::{TEST_TERMINAL_SIZE, diff_mode_fixture, handle, preview_test_app};
use crate::app::App;
use crate::presentation::app_mode::{AppMode, DiffCommentTarget, DiffFocus, DiffPreview};
use crate::runtime::mode::confirmation::{NO_OPTION_INDEX, YES_OPTION_INDEX};

/// Puts `app` in Diff mode with one completed file comment.
fn enter_diff_with_file_comment(app: &mut App) {
    app.mode = diff_mode_fixture(
        "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
        1,
        DiffFocus::Files,
        DiffPreview::default(),
    );
    if let AppMode::Diff { line_comments, .. } = &mut app.mode {
        line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
        line_comments
            .editing_input_mut()
            .expect("file comment should be editable")
            .insert_text("Split this module");
        line_comments.finish_editing();
        line_comments.start_selection(0);
    }
}

/// Presses one unmodified key in Diff mode.
fn press(app: &mut App, key_code: KeyCode) {
    handle(
        app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(key_code, KeyModifiers::NONE),
    );
}

/// Returns the open confirmation's highlighted option, if any.
fn submit_confirmation_index(app: &App) -> Option<usize> {
    match &app.mode {
        AppMode::Diff { line_comments, .. } => line_comments.submit_confirmation_index,
        _ => None,
    }
}

#[tokio::test]
async fn test_leaving_diff_with_comments_asks_to_submit_and_cancel_stays() {
    for exit_key in [KeyCode::Char('q'), KeyCode::Esc] {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        enter_diff_with_file_comment(&mut app);

        // Act
        press(&mut app, exit_key);

        // Assert
        assert!(is_submit_confirmation_open(&app));
        assert_eq!(submit_confirmation_index(&app), Some(YES_OPTION_INDEX));
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. } if !line_comments.is_selecting()
        ));

        // Act
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Char('?'));

        // Assert
        assert_eq!(submit_confirmation_index(&app), Some(NO_OPTION_INDEX));

        // Act
        press(&mut app, KeyCode::Esc);

        // Assert
        assert!(!is_submit_confirmation_open(&app));
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. } if line_comments.comments.len() == 1
        ));
        assert!(app.diff_comment_progress.is_empty());
    }
}

#[tokio::test]
async fn test_confirming_submit_opens_prompt_with_comment_preview_text() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    enter_diff_with_file_comment(&mut app);
    press(&mut app, KeyCode::Char('q'));
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

    // Act
    let should_submit = should_submit_line_comments(&app, enter);
    handle(&mut app, TEST_TERMINAL_SIZE, enter);

    // Assert
    assert!(should_submit);
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. }
            if input.text() == "File comments:\n- src/main.rs: Split this module"
    ));
}

#[tokio::test]
async fn test_confirmation_submits_only_for_confirming_keys() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    enter_diff_with_file_comment(&mut app);
    press(&mut app, KeyCode::Char('q'));
    let key = |key_code| KeyEvent::new(key_code, KeyModifiers::NONE);

    // Act
    let confirms_with_y = should_submit_line_comments(&app, key(KeyCode::Char('y')));
    let confirms_with_enter = should_submit_line_comments(&app, key(KeyCode::Enter));
    let confirms_with_s = should_submit_line_comments(&app, key(KeyCode::Char('s')));
    let confirms_with_n = should_submit_line_comments(&app, key(KeyCode::Char('n')));

    // Assert
    assert!(confirms_with_y);
    assert!(confirms_with_enter);
    assert!(!confirms_with_s);
    assert!(!confirms_with_n);
    assert_eq!(submit_confirmation_index(&app), Some(YES_OPTION_INDEX));
}

#[tokio::test]
async fn test_declining_submit_leaves_diff_and_keeps_comment_drafts() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    enter_diff_with_file_comment(&mut app);
    press(&mut app, KeyCode::Char('q'));

    // Act
    press(&mut app, KeyCode::Char('n'));

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::View { session_id, .. } if session_id == "session-id"
    ));
    let saved_comments = app
        .diff_comment_progress
        .get("session-id")
        .expect("declined comments should stay saved as drafts");
    assert_eq!(saved_comments.comments[0].input.text(), "Split this module");
    assert_eq!(saved_comments.submit_confirmation_index, None);
}

#[tokio::test]
async fn test_leaving_diff_skips_confirmation_when_session_cannot_receive_comments() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    enter_diff_with_file_comment(&mut app);
    if let AppMode::Diff { session_id, .. } = &mut app.mode {
        *session_id = "missing-session".into();
    }

    // Act
    press(&mut app, KeyCode::Char('q'));

    // Assert
    assert!(!is_submit_confirmation_open(&app));
    assert!(matches!(
        &app.mode,
        AppMode::View { session_id, .. } if session_id == "missing-session"
    ));
}

#[tokio::test]
async fn test_failed_confirmed_submit_closes_dialog_and_stays_in_diff() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    enter_diff_with_file_comment(&mut app);
    press(&mut app, KeyCode::Char('q'));
    if let AppMode::Diff { session_id, .. } = &mut app.mode {
        *session_id = "missing-session".into();
    }

    // Act
    press(&mut app, KeyCode::Char('y'));

    // Assert
    assert!(!is_submit_confirmation_open(&app));
    assert!(matches!(app.mode, AppMode::Diff { .. }));
}
