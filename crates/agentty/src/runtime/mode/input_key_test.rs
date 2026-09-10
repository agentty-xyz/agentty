use crossterm::event;
use crossterm::event::{KeyCode, KeyEvent};

use super::{
    InputCapabilities, command_for_key, is_control_key, is_control_newline_key,
    is_cursor_on_first_line, is_cursor_on_last_line, is_enter_key, is_insertable_char_key,
    is_line_delete_backspace, is_word_delete_backspace, normalize_pasted_text,
    normalize_single_line_pasted_text, should_insert_newline,
};
use crate::domain::input::{InputCommand, InputState};

// -----------------------------------------------------------------------
// should_insert_newline
// -----------------------------------------------------------------------

#[test]
fn test_should_insert_newline_for_alt_enter() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::ALT);

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(result);
}

#[test]
fn test_should_insert_newline_for_alt_shift_enter() {
    // Arrange
    let key = KeyEvent::new(
        KeyCode::Enter,
        event::KeyModifiers::ALT | event::KeyModifiers::SHIFT,
    );

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(result);
}

#[test]
fn test_should_insert_newline_for_alt_carriage_return() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('\r'), event::KeyModifiers::ALT);

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(result);
}

#[test]
fn test_should_insert_newline_for_alt_line_feed() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('\n'), event::KeyModifiers::ALT);

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(result);
}

#[test]
fn test_should_not_insert_newline_for_plain_enter() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::NONE);

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(!result);
}

#[test]
fn test_should_insert_newline_for_shift_enter() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::SHIFT);

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(result);
}

#[test]
fn test_should_insert_newline_for_shift_carriage_return() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('\r'), event::KeyModifiers::SHIFT);

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(result);
}

#[test]
fn test_should_insert_newline_for_shift_line_feed() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('\n'), event::KeyModifiers::SHIFT);

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(result);
}

#[test]
fn test_should_not_insert_newline_for_control_enter() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::CONTROL);

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(!result);
}

#[test]
fn test_should_not_insert_newline_for_non_enter_key() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('x'), event::KeyModifiers::SHIFT);

    // Act
    let result = should_insert_newline(key);

    // Assert
    assert!(!result);
}

// -----------------------------------------------------------------------
// is_enter_key
// -----------------------------------------------------------------------

#[test]
fn test_is_enter_key_for_enter() {
    // Arrange & Act
    let result = is_enter_key(KeyCode::Enter);

    // Assert
    assert!(result);
}

#[test]
fn test_is_enter_key_for_carriage_return() {
    // Arrange & Act
    let result = is_enter_key(KeyCode::Char('\r'));

    // Assert
    assert!(result);
}

#[test]
fn test_is_enter_key_for_line_feed() {
    // Arrange & Act
    let result = is_enter_key(KeyCode::Char('\n'));

    // Assert
    assert!(result);
}

#[test]
fn test_is_enter_key_for_other_key() {
    // Arrange & Act
    let result = is_enter_key(KeyCode::Char('x'));

    // Assert
    assert!(!result);
}

// -----------------------------------------------------------------------
// is_control_newline_key
// -----------------------------------------------------------------------

#[test]
fn test_is_control_newline_key_accepts_ctrl_j() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::CONTROL);

    // Act
    let result = is_control_newline_key(key, 'j');

    // Assert
    assert!(result);
}

#[test]
fn test_is_control_newline_key_accepts_ctrl_m() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('m'), event::KeyModifiers::CONTROL);

    // Act
    let result = is_control_newline_key(key, 'm');

    // Assert
    assert!(result);
}

#[test]
fn test_is_control_newline_key_rejects_plain_j() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::NONE);

    // Act
    let result = is_control_newline_key(key, 'j');

    // Assert
    assert!(!result);
}

// -----------------------------------------------------------------------
// is_word_delete_backspace / is_line_delete_backspace
// -----------------------------------------------------------------------

#[test]
fn test_is_word_delete_backspace_accepts_alt_modifier() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::ALT);

    // Act
    let result = is_word_delete_backspace(key);

    // Assert
    assert!(result);
}

#[test]
fn test_is_word_delete_backspace_rejects_plain_backspace() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::NONE);

    // Act
    let result = is_word_delete_backspace(key);

    // Assert
    assert!(!result);
}

#[test]
fn test_is_line_delete_backspace_accepts_super_modifier() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::SUPER);

    // Act
    let result = is_line_delete_backspace(key);

    // Assert
    assert!(result);
}

#[test]
fn test_is_line_delete_backspace_rejects_plain_backspace() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::NONE);

    // Act
    let result = is_line_delete_backspace(key);

    // Assert
    assert!(!result);
}

// -----------------------------------------------------------------------
// is_control_key
// -----------------------------------------------------------------------

#[test]
fn test_is_control_key_accepts_ctrl() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('u'), event::KeyModifiers::CONTROL);

    // Act & Assert
    assert!(is_control_key(key));
}

#[test]
fn test_is_control_key_rejects_plain() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('u'), event::KeyModifiers::NONE);

    // Act & Assert
    assert!(!is_control_key(key));
}

// -----------------------------------------------------------------------
// is_insertable_char_key
// -----------------------------------------------------------------------

#[test]
fn test_is_insertable_char_key_accepts_none() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('a'), event::KeyModifiers::NONE);

    // Act & Assert
    assert!(is_insertable_char_key(key));
}

#[test]
fn test_is_insertable_char_key_accepts_shift() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('A'), event::KeyModifiers::SHIFT);

    // Act & Assert
    assert!(is_insertable_char_key(key));
}

#[test]
fn test_is_insertable_char_key_rejects_control() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('a'), event::KeyModifiers::CONTROL);

    // Act & Assert
    assert!(!is_insertable_char_key(key));
}

// -----------------------------------------------------------------------
// is_cursor_on_first_line / is_cursor_on_last_line
// -----------------------------------------------------------------------

#[test]
fn test_is_cursor_on_first_line_at_start() {
    // Arrange
    let mut input = InputState::with_text("hello\nworld".to_string());
    input.cursor = 0;

    // Act & Assert
    assert!(is_cursor_on_first_line(&input));
}

#[test]
fn test_is_cursor_on_first_line_after_newline() {
    // Arrange
    let mut input = InputState::with_text("hello\nworld".to_string());
    input.cursor = "hello\nw".chars().count();

    // Act & Assert
    assert!(!is_cursor_on_first_line(&input));
}

#[test]
fn test_is_cursor_on_last_line_at_end() {
    // Arrange
    let mut input = InputState::with_text("hello\nworld".to_string());
    input.cursor = "hello\nworld".chars().count();

    // Act & Assert
    assert!(is_cursor_on_last_line(&input));
}

#[test]
fn test_is_cursor_on_last_line_before_newline() {
    // Arrange
    let mut input = InputState::with_text("hello\nworld".to_string());
    input.cursor = 3;

    // Act & Assert
    assert!(!is_cursor_on_last_line(&input));
}

#[test]
fn test_command_for_key_maps_shared_editing_shortcuts() {
    // Arrange
    let cases = [
        (
            KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::ALT),
            InputCommand::DeleteWordBackward,
        ),
        (
            KeyEvent::new(KeyCode::Backspace, event::KeyModifiers::SHIFT),
            InputCommand::DeleteWordBackward,
        ),
        (
            KeyEvent::new(KeyCode::Left, event::KeyModifiers::SHIFT),
            InputCommand::MoveWordLeft,
        ),
        (
            KeyEvent::new(KeyCode::Right, event::KeyModifiers::SHIFT),
            InputCommand::MoveWordRight,
        ),
        (
            KeyEvent::new(KeyCode::Char('w'), event::KeyModifiers::CONTROL),
            InputCommand::DeleteWordBackward,
        ),
        (
            KeyEvent::new(KeyCode::Char('z'), event::KeyModifiers::CONTROL),
            InputCommand::Undo,
        ),
        (
            KeyEvent::new(KeyCode::Char('y'), event::KeyModifiers::CONTROL),
            InputCommand::Redo,
        ),
        (
            KeyEvent::new(KeyCode::Delete, event::KeyModifiers::NONE),
            InputCommand::DeleteForward,
        ),
        (
            KeyEvent::new(KeyCode::Right, event::KeyModifiers::NONE),
            InputCommand::MoveRight,
        ),
        (
            KeyEvent::new(KeyCode::Home, event::KeyModifiers::NONE),
            InputCommand::MoveHome,
        ),
        (
            KeyEvent::new(KeyCode::End, event::KeyModifiers::NONE),
            InputCommand::MoveEnd,
        ),
        (
            KeyEvent::new(
                KeyCode::Char('Z'),
                event::KeyModifiers::CONTROL | event::KeyModifiers::SHIFT,
            ),
            InputCommand::Redo,
        ),
        (
            KeyEvent::new(KeyCode::Char('u'), event::KeyModifiers::CONTROL),
            InputCommand::DeleteCurrentLine,
        ),
    ];

    // Act & Assert
    for (key, expected) in cases {
        assert_eq!(
            command_for_key(key, InputCapabilities::SINGLE_LINE),
            Some(expected)
        );
    }
}

#[test]
fn test_command_for_key_respects_multiline_capability() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Enter, event::KeyModifiers::SHIFT);

    // Act
    let multiline_command = command_for_key(key, InputCapabilities::MULTILINE);
    let single_line_command = command_for_key(key, InputCapabilities::SINGLE_LINE);

    // Assert
    assert_eq!(multiline_command, Some(InputCommand::InsertNewline));
    assert_eq!(single_line_command, None);
}

// -----------------------------------------------------------------------
// normalize_pasted_text
// -----------------------------------------------------------------------

#[test]
fn test_normalize_pasted_text_replaces_carriage_returns() {
    // Arrange
    let pasted_text = "line 1\r\nline 2\rline 3\nline 4";

    // Act
    let normalized = normalize_pasted_text(pasted_text);

    // Assert
    assert_eq!(normalized, "line 1\nline 2\nline 3\nline 4");
}

#[test]
fn test_normalize_pasted_text_preserves_plain_newlines() {
    // Arrange
    let pasted_text = "line 1\nline 2\nline 3";

    // Act
    let normalized = normalize_pasted_text(pasted_text);

    // Assert
    assert_eq!(normalized, pasted_text);
}

#[test]
fn test_normalize_single_line_pasted_text_keeps_first_line() {
    // Arrange
    let pasted_text = "feature/shared-input\r\nignored";

    // Act
    let normalized = normalize_single_line_pasted_text(pasted_text);

    // Assert
    assert_eq!(normalized, "feature/shared-input");
}
