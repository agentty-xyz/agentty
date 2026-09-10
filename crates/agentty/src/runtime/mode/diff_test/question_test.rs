use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::open_line_comment_prompt;
use super::support::{TEST_TERMINAL_SIZE, handle};
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, DiffRestoreTarget, HelpContext,
    QuestionModeSnapshot,
};
use crate::runtime::EventResult;

#[tokio::test]
async fn test_handle_question_mark_opens_help_overlay() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: "diff output".to_string(),
        scroll_offset: 5,
        file_explorer_selected_index: 3,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::Ready {
            content: "# Preview".to_string(),
            path: "README.md".to_string(),
            request_id: 6,
        },
        review_comments: None,
        restore: None,
        scroll_cache: None,
    };

    // Act
    let event_result = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::Help {
            context: HelpContext::Diff {
                ref session_id,
                ref diff,
                scroll_offset: 5,
                file_explorer_selected_index: 3,
                focus: DiffFocus::Files,
                selected_diff_line_index: 0,
                preview: DiffPreview::Ready { request_id: 6, .. },
                ..
            },
            scroll_offset: 0,
        } if session_id == "session-id" && diff == "diff output"
    ));
}

#[tokio::test]
async fn test_handle_question_then_help_then_exit_preserves_restore_question() {
    // Arrange — diff opened from question mode, then user opens help with
    // `?`.

    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let snapshot = QuestionModeSnapshot {
        at_mention_state: None,
        current_index: 1,
        input: InputState::default(),
        questions: vec![
            QuestionItem {
                options: Vec::new(),
                text: "Q1?".to_string(),
            },
            QuestionItem {
                options: Vec::new(),
                text: "Q2?".to_string(),
            },
        ],
        responses: vec!["answer-1".to_string()],
        scroll_offset: None,
        selected_option_index: None,
        session_id: "session-q".into(),
    };

    app.mode = AppMode::Diff {
        session_id: "session-q".into(),
        diff: "diff output".to_string(),
        scroll_offset: 3,
        file_explorer_selected_index: 1,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: Some(Box::new(DiffRestoreTarget::Question(snapshot))),
        scroll_cache: None,
    };

    // Act — open help overlay.
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );

    // Intermediate assert — help carries the snapshot.
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

    // Intermediate assert — diff still carries the snapshot.
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

    // Assert — restored to Question mode, not View.
    assert!(matches!(
        app.mode,
        AppMode::Question {
            ref session_id,
            current_index: 1,
            focus: crate::presentation::app_mode::ChatFocus::Input,
            ..
        } if session_id == "session-q"
    ));
}

#[tokio::test]
async fn test_handle_question_mark_in_non_diff_mode_leaves_mode_unchanged() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::List;

    // Act
    let event_result = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );

    // Assert — the help key is a no-op outside diff mode.
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_quit_with_question_snapshot_restores_question_mode() {
    // Arrange — diff opened from question mode carries a snapshot.

    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-q".into(),
        diff: "diff output".to_string(),
        scroll_offset: 0,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: Some(Box::new(DiffRestoreTarget::Question(
            QuestionModeSnapshot {
                at_mention_state: None,
                current_index: 0,
                input: InputState::default(),
                questions: vec![QuestionItem {
                    options: Vec::new(),
                    text: "Q?".to_string(),
                }],
                responses: Vec::new(),
                scroll_offset: None,
                selected_option_index: None,
                session_id: "session-q".into(),
            },
        ))),
        scroll_cache: None,
    };

    // Act — question-origin diffs cannot become text prompt composers.
    open_line_comment_prompt(&mut app);

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            restore: Some(restore),
            ..
        } if matches!(restore.as_ref(), DiffRestoreTarget::Question(_))
    ));

    // Act
    let event_result = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert — restored to Question mode, not View.
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::Question {
            ref session_id,
            focus: crate::presentation::app_mode::ChatFocus::Input,
            ..
        } if session_id == "session-q"
    ));
}
