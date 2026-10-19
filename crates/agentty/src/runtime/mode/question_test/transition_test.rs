use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::support::{TEST_TERMINAL_SIZE, handle, question_mode_with_options};
use crate::domain::input::InputState;
use crate::domain::question::{QuestionItem, default_option_index};
use crate::presentation::app_mode::{AppMode, ChatFocus};

#[tokio::test]
async fn test_handle_q_returns_to_sessions_list_in_chat_focus() {
    // Arrange — chat focus is read-only, so plain `q` should jump to the
    // sessions list (matching session-view navigation).
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-q-chat".into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: "Q?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Chat,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    )
    .await;

    // Assert — switched to the sessions list.
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_q_returns_to_sessions_list_when_navigating_options() {
    // Arrange — option-navigation mode treats letters as navigation
    // keys, so `q` should exit to the sessions list.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-q-options".into(),
        questions: vec![QuestionItem {
            options: vec!["Yes".to_string(), "No".to_string()],
            text: "Need a target branch?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: Some(0),
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    )
    .await;

    // Assert — switched to the sessions list.
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_q_saves_partial_answers_for_reopen() {
    // Arrange — first question already answered, second in option
    // navigation. `q` must keep the submitted answer for the next visit.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-q-save".into(),
        questions: vec![
            QuestionItem::with_options("First?", vec!["Yes".to_string(), "No".to_string()]),
            QuestionItem::with_options("Second?", vec!["A".to_string(), "B".to_string()]),
        ],
        responses: vec!["Yes".to_string()],
        current_index: 1,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: Some(1),
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    )
    .await;

    // Assert — list mode plus saved progress for the session.
    assert!(matches!(app.mode, AppMode::List));
    let progress = app
        .question_progress
        .get("session-q-save")
        .expect("progress should be saved");
    assert_eq!(progress.responses, vec!["Yes".to_string()]);
    assert_eq!(progress.current_index, 1);
    assert_eq!(progress.selected_option_index, Some(1));
}

#[tokio::test]
async fn test_handle_q_inserts_character_in_free_text_answer() {
    // Arrange — free-text mode (no option navigation) must accept `q` as
    // a regular character so users can type answers containing it.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-q-text".into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: "Free text?".to_string(),
        }],
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
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    )
    .await;

    // Assert — character inserted into input, mode unchanged.
    assert!(matches!(
        &app.mode,
        AppMode::Question { input, .. } if input.text() == "q"
    ));
}

#[tokio::test]
async fn test_handle_down_from_first_selects_second_option() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            selected_option_index: Some(1),
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_ctrl_c_ends_turn_and_transitions_to_view() {
    // Arrange — two unanswered questions. Ctrl+C should cancel the
    // question turn and transition to View without sending a reply.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.review_cache.insert(
        "session-ctrl-c".into(),
        crate::app::ReviewCacheEntry::Ready {
            request_id: uuid::Uuid::nil(),
            text: "Focused review".to_string(),
            diff_hash: 42,
        },
    );
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-ctrl-c".into(),
        questions: vec![
            QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "First question?".to_string(),
            },
            QuestionItem {
                options: vec!["A".to_string(), "B".to_string()],
                text: "Second question?".to_string(),
            },
        ],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("partial answer".to_string()),
        scroll_offset: None,
        selected_option_index: Some(0),
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert — transitions to View mode with no reply sent.
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            ..
        } if session_id == "session-ctrl-c"
    ));
}

#[tokio::test]
async fn test_handle_ctrl_c_is_swallowed_while_chat_is_focused() {
    // Arrange — chat focus is read-only, so Ctrl+C must not end the turn
    // while the user scrolls the transcript.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "session-chat-ctrl-c".into(),
        questions: vec![QuestionItem {
            options: Vec::new(),
            text: "Q?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Chat,
        input: InputState::with_text("partial answer".to_string()),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert — the question turn stays active with the draft preserved.
    assert!(matches!(
        app.mode,
        AppMode::Question {
            focus: ChatFocus::Chat,
            ref input,
            ..
        } if input.text() == "partial answer"
    ));
}

#[tokio::test]
async fn test_handle_tab_toggles_focus_from_answer_to_chat() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            focus: ChatFocus::Chat,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_tab_toggles_focus_from_chat_to_answer() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();
    if let AppMode::Question { focus, .. } = &mut app.mode {
        *focus = ChatFocus::Chat;
    }

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            focus: ChatFocus::Input,
            ..
        }
    ));
}

#[test]
fn test_default_option_index_returns_first_when_options_exist() {
    // Arrange
    let questions = vec![QuestionItem {
        options: vec!["A".to_string(), "B".to_string()],
        text: "Pick?".to_string(),
    }];

    // Act & Assert
    assert_eq!(default_option_index(&questions, 0), Some(0));
}

#[test]
fn test_default_option_index_returns_none_for_out_of_bounds() {
    // Arrange
    let questions: Vec<QuestionItem> = Vec::new();

    // Act & Assert
    assert_eq!(default_option_index(&questions, 0), None);
}

#[tokio::test]
async fn test_handle_enter_with_selected_option_submits_option_text() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "missing-session".into(),
        questions: vec![
            QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Continue?".to_string(),
            },
            QuestionItem {
                options: vec!["Details".to_string(), "Skip".to_string()],
                text: "Follow-up?".to_string(),
            },
        ],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: Some(1),
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
        } if responses == &vec!["No".to_string()]
    ));
}

#[tokio::test]
async fn test_handle_jump_to_top_in_chat_focus_sets_offset_zero() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();
    if let AppMode::Question {
        focus,
        scroll_offset,
        ..
    } = &mut app.mode
    {
        *focus = ChatFocus::Chat;
        *scroll_offset = None;
    }

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            focus: ChatFocus::Chat,
            scroll_offset: Some(0),
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_jump_to_bottom_in_chat_focus_sets_offset_none() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();
    if let AppMode::Question {
        focus,
        scroll_offset,
        ..
    } = &mut app.mode
    {
        *focus = ChatFocus::Chat;
        *scroll_offset = Some(5);
    }

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            focus: ChatFocus::Chat,
            scroll_offset: None,
            ..
        }
    ));
}

#[tokio::test]
async fn test_alt_enter_ignored_while_navigating_options() {
    // Arrange — navigating options, Alt+Enter should submit (not insert
    // newline), because newline insertion only applies in free-text mode.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "missing-session".into(),
        questions: vec![
            QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Continue?".to_string(),
            },
            QuestionItem {
                options: vec!["A".to_string()],
                text: "Follow-up?".to_string(),
            },
        ],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: Some(0),
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
    )
    .await;

    // Assert — option was submitted, advanced to next question.
    assert!(matches!(
        app.mode,
        AppMode::Question {
            current_index: 1,
            ref responses,
            ..
        } if responses == &vec!["Yes".to_string()]
    ));
}
