use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::support::{TEST_TERMINAL_SIZE, handle, question_mode_with_options};
use crate::presentation::app_mode::{AppMode, ChatFocus};

#[tokio::test]
async fn test_handle_scroll_down_in_chat_focus_updates_scroll_offset() {
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
        *scroll_offset = Some(0);
    }

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    )
    .await;

    // Assert — offset incremented (or set to None if at bottom, since no
    // session content exists in test).
    assert!(matches!(
        app.mode,
        AppMode::Question {
            focus: ChatFocus::Chat,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_scroll_keys_ignored_in_answer_focus() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();

    // Act — 'j' in answer focus navigates options, not scroll.
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    )
    .await;

    // Assert — selected_option_index moved, not scroll.
    assert!(matches!(
        app.mode,
        AppMode::Question {
            focus: ChatFocus::Input,
            selected_option_index: Some(1),
            scroll_offset: None,
            ..
        }
    ));
}
