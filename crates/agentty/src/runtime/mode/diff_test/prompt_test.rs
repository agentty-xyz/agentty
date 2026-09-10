use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{
    apply_comment_input_key, handle_line_comment_edit_key, handle_paste, open_line_comment_prompt,
    should_submit_line_comments, sync_comment_at_mention,
};
use super::support::{
    RESTORE_DRAFT_TEXT, TEST_TERMINAL_SIZE, assert_restored_prompt_composer, diff_mode_fixture,
    handle, non_default_prompt_snapshot, preview_test_app,
};
use crate::domain::input::InputState;
use crate::presentation::app_mode::{
    AppMode, DiffCommentTarget, DiffFocus, DiffLineCommentAnchor, DiffLineCommentTarget,
    DiffLineComments, DiffPreview, DiffRestoreTarget, HelpContext, PromptModeSnapshot,
};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};
use crate::runtime::EventResult;

#[tokio::test]
async fn test_open_line_comment_prompt_handles_mode_and_prompt_restore() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    app.mode = AppMode::List;

    // Act
    open_line_comment_prompt(&mut app);

    // Assert
    assert!(matches!(app.mode, AppMode::List));

    // Arrange
    app.mode = diff_mode_fixture(
        "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
        1,
        DiffFocus::Content,
        DiffPreview::default(),
    );
    if let AppMode::Diff {
        line_comments,
        restore,
        ..
    } = &mut app.mode
    {
        line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
            content: "review();".to_string(),
            line: 1,
            path: "src/main.rs".to_string(),
            side: crate::presentation::app_mode::DiffLineSide::New,
        }));
        line_comments
            .editing_input_mut()
            .expect("comment should be editable")
            .insert_text("Explain this call");
        line_comments.finish_editing();
        *restore = Some(Box::new(DiffRestoreTarget::Prompt(
            non_default_prompt_snapshot(),
        )));
    }

    // Act
    open_line_comment_prompt(&mut app);

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. }
            if input.text().starts_with(RESTORE_DRAFT_TEXT)
                && input.text().ends_with("Explain this call")
    ));

    // Arrange
    app.mode = diff_mode_fixture(
        "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
        1,
        DiffFocus::Content,
        DiffPreview::default(),
    );
    if let AppMode::Diff { session_id, .. } = &mut app.mode {
        *session_id = "missing-session".into();
    }

    // Act
    open_line_comment_prompt(&mut app);

    // Assert
    assert!(matches!(app.mode, AppMode::Diff { .. }));
}

#[tokio::test]
async fn test_handle_paste_preserves_multiline_diff_comment() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let diff = "diff --git a/src/main.rs b/src/main.rs\n+review();\n";
    app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Act
    handle_paste(&mut app, "first line\r\nsecond line");

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.comments[0].input.text() == "first line\nsecond line"
    ));
}

#[test]
#[should_panic(expected = "expected AppMode::Prompt after leaving diff")]
fn test_assert_restored_prompt_composer_rejects_non_prompt_mode() {
    // Arrange, Act & Assert — the helper rejects modes that are not a
    // restored composer.
    assert_restored_prompt_composer(&AppMode::List);
}

#[tokio::test]
async fn test_line_comment_paste_ignores_non_editing_modes() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

    // Act
    handle_paste(&mut app, "ignored");
    let should_submit =
        should_submit_line_comments(&app, KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert!(!should_submit);

    // Arrange
    app.mode = diff_mode_fixture(
        "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
        1,
        DiffFocus::Content,
        DiffPreview::default(),
    );

    // Act
    handle_paste(&mut app, "ignored");

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. } if line_comments.comments.is_empty()
    ));
}

#[tokio::test]
async fn test_handle_prompt_then_help_then_exit_preserves_composer_context() {
    // Arrange — diff opened from prompt mode, then the user opens help with
    // `?`.
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-p".into(),
        diff: "diff output".to_string(),
        scroll_offset: 3,
        file_explorer_selected_index: 1,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: Some(Box::new(DiffRestoreTarget::Prompt(
            non_default_prompt_snapshot(),
        ))),
        scroll_cache: None,
    };

    // Act — open help overlay.
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );

    // Intermediate assert — help carries the prompt restore target.
    assert!(matches!(
        app.mode,
        AppMode::Help {
            context: HelpContext::Diff {
                restore: Some(_),
                ..
            },
            ..
        }
    ));

    // Act — close help overlay, returning to diff.
    crate::runtime::mode::help::handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    // Intermediate assert — diff still carries the prompt restore target.
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            restore: Some(_),
            ..
        }
    ));

    // Act — exit diff.
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );

    // Assert — restored to the prompt composer with all context intact.
    assert_restored_prompt_composer(&app.mode);
}

#[tokio::test]
async fn test_comment_lookup_edits_paste_and_newline() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    sync_comment_at_mention(&mut app);
    apply_comment_input_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    app.mode = diff_mode_fixture(
        "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
        1,
        DiffFocus::Content,
        DiffPreview::default(),
    );
    sync_comment_at_mention(&mut app);
    if let AppMode::Diff { line_comments, .. } = &mut app.mode {
        line_comments.start_editing_target(DiffCommentTarget::File {
            path: "src/main.rs".into(),
        });
    }

    // Act
    handle_paste(&mut app, "@sr");
    handle_line_comment_edit_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
    );

    // Assert
    assert!(
        matches!(&app.mode, AppMode::Diff { line_comments, .. } if line_comments.at_mention_state.is_some())
    );

    // Act
    handle_line_comment_edit_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

    // Assert
    assert!(
        matches!(&app.mode, AppMode::Diff { line_comments, .. } if line_comments.at_mention_state.is_none() && line_comments.comments[0].input.text() == "@src\n" && line_comments.is_editing())
    );
}

#[tokio::test]
async fn test_handle_quit_with_prompt_snapshot_restores_prompt_mode() {
    // Arrange — diff opened from prompt mode carries a composer snapshot.

    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-p".into(),
        diff: "diff output".to_string(),
        scroll_offset: 0,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: Some(Box::new(DiffRestoreTarget::Prompt(PromptModeSnapshot {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::new(Vec::new()),
            input: InputState::with_text("draft text".to_string()),
            scroll_offset: None,
            session_id: "session-p".into(),
            slash_state: PromptSlashState::default(),
        }))),
        scroll_cache: None,
    };

    // Act
    let event_result = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert — restored to prompt mode with the draft intact and input
    // focus.
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            focus: crate::presentation::app_mode::ChatFocus::Input,
            input,
            session_id,
            ..
        } if input.text() == "draft text" && session_id == "session-p"
    ));
}

#[tokio::test]
async fn test_handle_quit_with_prompt_snapshot_preserves_composer_context() {
    // Arrange — a prompt snapshot carrying non-default attachment, history,
    // slash, and at-mention state so leaving diff cannot silently drop it.
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-p".into(),
        diff: "diff output".to_string(),
        scroll_offset: 0,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: Some(Box::new(DiffRestoreTarget::Prompt(
            non_default_prompt_snapshot(),
        ))),
        scroll_cache: None,
    };

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert — every composer field survives the diff round-trip.
    assert_restored_prompt_composer(&app.mode);
}
