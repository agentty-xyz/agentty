use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::support::{
    TEST_TERMINAL_SIZE, free_text_question_mode, handle, question_mode_with_options,
};
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::PromptAtMentionState;

#[tokio::test]
async fn test_handle_escape_preserves_question_turn_and_draft() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-esc".into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: "Q?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("partial answer".to_string()),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            ref session_id,
            ref input,
            ref responses,
            ..
        } if session_id == "session-esc"
            && input.text() == "partial answer"
            && responses.is_empty()
    ));
}

#[tokio::test]
async fn test_question_lookup_escape_preserves_draft_and_restores_tab_focus_toggle() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();
    if let AppMode::Question {
        at_mention_state,
        input,
        selected_option_index,
        ..
    } = &mut app.mode
    {
        *selected_option_index = None;
        *input = InputState::with_text("@src".to_string());
        *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
    }

    // Act
    for code in [KeyCode::Esc, KeyCode::Tab] {
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(code, KeyModifiers::NONE),
        )
        .await;
    }

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Question {
            at_mention_state: None,
            focus: ChatFocus::Chat,
            input,
            ..
        } if input.text() == "@src"
    ));
}

#[tokio::test]
async fn test_resolve_free_text_ctrl_d_deletes_forward() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = free_text_question_mode("hello", 2);

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert
    if let AppMode::Question { input, .. } = &app.mode {
        assert_eq!(input.text(), "helo");
        assert_eq!(input.cursor, 2);
    }
}

#[tokio::test]
async fn test_resolve_free_text_ctrl_w_deletes_previous_word() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = free_text_question_mode("hello brave world", "hello brave world".chars().count());

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert — deletes "world" and the preceding whitespace.
    if let AppMode::Question { input, .. } = &app.mode {
        assert_eq!(input.text(), "hello brave");
    }
}

#[tokio::test]
async fn test_resolve_free_text_alt_backspace_deletes_previous_word() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = free_text_question_mode("hello brave world", "hello brave world".chars().count());

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
    )
    .await;

    // Assert — deletes "world" and the preceding whitespace.
    if let AppMode::Question { input, .. } = &app.mode {
        assert_eq!(input.text(), "hello brave");
    }
}

#[tokio::test]
async fn test_resolve_free_text_super_backspace_deletes_current_line() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = free_text_question_mode("first\nsecond\nthird", "first\nseco".chars().count());

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::SUPER),
    )
    .await;

    // Assert — current line "second" is deleted.
    if let AppMode::Question { input, .. } = &app.mode {
        assert!(!input.text().contains("second"));
    }
}
