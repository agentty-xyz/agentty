use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{QuestionAction, resolve_question_action};
use super::support::{free_text_question_mode, question_mode_with_options};
use crate::domain::input::InputState;
use crate::presentation::app_mode::AppMode;

#[test]
fn free_text_keys_move_cursor() {
    // Arrange
    for (text, cursor, code, modifiers, expected) in [
        (
            "first\nsecond\nthird",
            10,
            KeyCode::Left,
            KeyModifiers::SUPER,
            6,
        ),
        (
            "first\nsecond\nthird",
            8,
            KeyCode::Right,
            KeyModifiers::SUPER,
            12,
        ),
        (
            "first\nsecond\nthird",
            10,
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
            6,
        ),
        (
            "first\nsecond\nthird",
            8,
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
            12,
        ),
        (
            "hello brave world",
            17,
            KeyCode::Char('b'),
            KeyModifiers::ALT,
            12,
        ),
        (
            "hello brave world",
            0,
            KeyCode::Char('f'),
            KeyModifiers::ALT,
            6,
        ),
        (
            "hello brave world",
            17,
            KeyCode::Left,
            KeyModifiers::ALT,
            12,
        ),
        ("hello brave world", 0, KeyCode::Right, KeyModifiers::ALT, 6),
        ("hello", 2, KeyCode::Char('f'), KeyModifiers::CONTROL, 3),
        ("hello", 3, KeyCode::Char('b'), KeyModifiers::CONTROL, 2),
        (
            "first\nsecond",
            10,
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
            4,
        ),
        (
            "first\nsecond",
            2,
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
            8,
        ),
    ] {
        let mut mode = free_text_question_mode(text, cursor);
        let key = KeyEvent::new(code, modifiers);

        // Act
        let action = resolve_question_action(&mut mode, key);

        // Assert
        assert!(matches!(action, Some(QuestionAction::Continue)), "{key:?}");
        let AppMode::Question { input, .. } = mode else {
            unreachable!("editing keeps question mode");
        };
        assert_eq!(input.cursor, expected, "{key:?}");
        assert_eq!(input.text(), text, "{key:?}");
    }
}

#[test]
fn free_text_keys_edit_without_submitting() {
    // Arrange
    for (text, cursor, code, modifiers, expected) in [
        ("hello", 5, KeyCode::Enter, KeyModifiers::ALT, "hello\n"),
        ("hello", 5, KeyCode::Enter, KeyModifiers::SHIFT, "hello\n"),
        (
            "hello",
            5,
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
            "hello\n",
        ),
        (
            "hello",
            5,
            KeyCode::Char('m'),
            KeyModifiers::CONTROL,
            "hello\n",
        ),
        (
            "first\nsecond\nthird",
            8,
            KeyCode::Char('k'),
            KeyModifiers::CONTROL,
            "first\nse\nthird",
        ),
        (
            "hello brave world",
            17,
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
            "hello brave",
        ),
        (
            "hello brave world",
            17,
            KeyCode::Backspace,
            KeyModifiers::ALT,
            "hello brave",
        ),
        (
            "first\nsecond\nthird",
            10,
            KeyCode::Backspace,
            KeyModifiers::SUPER,
            "first\nthird",
        ),
    ] {
        let mut mode = free_text_question_mode(text, cursor);
        let key = KeyEvent::new(code, modifiers);

        // Act
        let action = resolve_question_action(&mut mode, key);

        // Assert
        assert!(matches!(action, Some(QuestionAction::Continue)), "{key:?}");
        let AppMode::Question { input, .. } = mode else {
            unreachable!("editing keeps question mode");
        };
        assert_eq!(input.text(), expected, "{key:?}");
    }
}

#[test]
fn free_text_undo_restores_text_and_cursor() {
    // Arrange
    let mut mode = free_text_question_mode("hello brave", 11);
    resolve_question_action(
        &mut mode,
        KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
    );

    // Act
    let action = resolve_question_action(
        &mut mode,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
    );

    // Assert
    assert!(matches!(action, Some(QuestionAction::Continue)));
    let AppMode::Question { input, .. } = mode else {
        unreachable!("editing keeps question mode");
    };
    assert_eq!(input.text(), "hello brave");
    assert_eq!(input.cursor, 11);
}

#[test]
fn option_keys_preserve_selection_and_free_text_boundaries() {
    // Arrange
    for (selection, text, cursor, code, expected_selection, expected_text) in [
        (Some(2), "", 0, KeyCode::Down, None, ""),
        (None, "", 0, KeyCode::Down, Some(0), ""),
        (
            None,
            "first\nsecond",
            2,
            KeyCode::Down,
            None,
            "first\nsecond",
        ),
        (Some(0), "", 0, KeyCode::Up, None, ""),
        (None, "", 0, KeyCode::Up, Some(2), ""),
        (
            None,
            "first\nsecond",
            10,
            KeyCode::Up,
            None,
            "first\nsecond",
        ),
        (Some(0), "", 0, KeyCode::Char('j'), Some(1), ""),
        (Some(2), "", 0, KeyCode::Char('k'), Some(1), ""),
        (Some(1), "", 0, KeyCode::Char('x'), Some(1), ""),
        (None, "", 0, KeyCode::Char('x'), None, "x"),
    ] {
        let mut mode = question_mode_with_options();
        let AppMode::Question {
            input,
            selected_option_index,
            ..
        } = &mut mode
        else {
            unreachable!("fixture is question mode");
        };
        *input = InputState::with_text(text.to_string());
        input.cursor = cursor;
        *selected_option_index = selection;

        // Act
        let action = resolve_question_action(&mut mode, KeyEvent::new(code, KeyModifiers::NONE));

        // Assert
        assert!(matches!(action, Some(QuestionAction::Continue)), "{code:?}");
        let AppMode::Question {
            input,
            selected_option_index,
            ..
        } = mode
        else {
            unreachable!("navigation keeps question mode");
        };
        assert_eq!(selected_option_index, expected_selection, "{code:?}");
        assert_eq!(input.text(), expected_text, "{code:?}");
    }
}

#[test]
fn free_text_navigation_without_options_stays_in_input() {
    // Arrange
    let mut mode = free_text_question_mode("some text", 4);

    // Act
    let action = resolve_question_action(&mut mode, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

    // Assert
    assert!(matches!(action, Some(QuestionAction::Continue)));
    assert!(matches!(
        mode,
        AppMode::Question {
            selected_option_index: None,
            ..
        }
    ));
}

#[test]
fn question_input_ignores_other_modes() {
    // Arrange
    let mut mode = AppMode::List;

    // Act
    let action =
        resolve_question_action(&mut mode, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // Assert
    assert!(action.is_none());
    assert!(matches!(mode, AppMode::List));
}
