use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{NO_ANSWER, handle_paste};
use super::support::{TEST_TERMINAL_SIZE, handle, question_mode_with_options};
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::presentation::app_mode::{AppMode, ChatFocus};

#[tokio::test]
async fn test_handle_paste_normalizes_line_endings_in_free_text_mode() {
    // Arrange — free-text mode (user selected "Type custom answer").
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-id".into(),
        questions: vec![QuestionItem {
            options: vec!["Default".to_string()],
            text: "Question".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    handle_paste(&mut app, "line1\r\nline2\rline3");

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question { ref input, .. } if input.text() == "line1\nline2\nline3"
    ));
}

#[tokio::test]
async fn test_handle_paste_ignored_while_navigating_options() {
    // Arrange — navigating options mode.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();

    // Act
    handle_paste(&mut app, "pasted text");

    // Assert — input unchanged, selection unchanged.
    assert!(matches!(
        app.mode,
        AppMode::Question {
            selected_option_index: Some(0),
            ref input,
            ..
        } if input.text().is_empty()
    ));
}

#[tokio::test]
async fn test_handle_enter_on_type_custom_answer_with_blank_input_records_no_answer() {
    // Arrange — user navigated to "Type custom answer" and entered
    // free-text mode, then pressed Enter with empty input.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "missing-session".into(),
        questions: vec![
            QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Need a target branch?".to_string(),
            },
            QuestionItem {
                options: vec!["Unit".to_string(), "Integration".to_string()],
                text: "Need tests?".to_string(),
            },
        ],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            current_index: 1,
            ref responses,
            selected_option_index: Some(0),
            ..
        } if responses == &vec![NO_ANSWER.to_string()]
    ));
}
