//! Shared input key handling utilities used by both prompt and question modes.
//!
//! Contains the canonical `KeyEvent` to semantic input-command mapping plus
//! paste normalization shared across text-input modes.

use crossterm::event::{self, KeyCode, KeyEvent};

use crate::domain::input::{InputCommand, InputState};

/// Capabilities that differ between single-line and multiline inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InputCapabilities {
    multiline: bool,
}

impl InputCapabilities {
    pub(crate) const MULTILINE: Self = Self { multiline: true };
    pub(crate) const SINGLE_LINE: Self = Self { multiline: false };
}

// ---------------------------------------------------------------------------
// Modifier predicates
// ---------------------------------------------------------------------------

/// Returns true when `Ctrl` is pressed without `Alt` or `Shift`.
///
/// macOS terminals send `Ctrl+a` (`\x01`) for `Cmd+Left` and `Ctrl+e`
/// (`\x05`) for `Cmd+Right` because the legacy terminal protocol cannot
/// encode the Super/Cmd modifier.
pub(crate) fn is_control_key(key: KeyEvent) -> bool {
    key.modifiers == event::KeyModifiers::CONTROL
}

/// Returns true when the `Alt` modifier is present.
///
/// macOS terminals report `Option`+key as `Alt`+key. `Option`+`Left` sends
/// `ESC b` (parsed as `Alt+b`) and `Option`+`Right` sends `ESC f` (parsed
/// as `Alt+f`).
pub(crate) fn is_alt_key(key: KeyEvent) -> bool {
    key.modifiers.contains(event::KeyModifiers::ALT)
}

/// Returns true when backspace should delete the previous word instead of a
/// single character.
///
/// `Option`+`Backspace` is reported as `Alt` on macOS terminals. `Shift` is
/// also accepted for compatibility with the existing word-delete shortcut.
/// `Cmd`+`Backspace` is handled separately as a whole-line deletion shortcut.
pub(crate) fn is_word_delete_backspace(key: KeyEvent) -> bool {
    key.modifiers
        .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT)
}

/// Returns true when backspace should delete the current line content.
///
/// On macOS terminals this is produced by pressing `Cmd`+`Backspace`.
pub(crate) fn is_line_delete_backspace(key: KeyEvent) -> bool {
    key.modifiers.contains(event::KeyModifiers::SUPER)
}

/// Returns whether one key event inserts its character into input.
///
/// Only plain keys (no modifier) and `Shift`+key produce insertable
/// characters.
pub(crate) fn is_insertable_char_key(key: KeyEvent) -> bool {
    matches!(
        key.modifiers,
        event::KeyModifiers::NONE | event::KeyModifiers::SHIFT
    )
}

// ---------------------------------------------------------------------------
// Enter / newline predicates
// ---------------------------------------------------------------------------

/// Returns whether an Enter-like key event should insert a newline into the
/// input.
///
/// Both `Alt+Enter` and `Shift+Enter` are accepted so newline entry remains
/// portable across terminals that emit either modifier for multiline editing.
pub(crate) fn should_insert_newline(key: KeyEvent) -> bool {
    is_enter_key(key.code)
        && key
            .modifiers
            .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT)
}

/// Returns true when the key code represents an Enter key press.
///
/// Some terminals encode Enter as `\r` or `\n` character events rather than
/// `KeyCode::Enter`.
pub(crate) fn is_enter_key(key_code: KeyCode) -> bool {
    matches!(key_code, KeyCode::Enter | KeyCode::Char('\r' | '\n'))
}

/// Returns true when the key event represents a control-key newline variant
/// such as `Ctrl+j` or `Ctrl+m`.
pub(crate) fn is_control_newline_key(key: KeyEvent, character: char) -> bool {
    key.modifiers == event::KeyModifiers::CONTROL && matches!(character, 'j' | 'm' | '\n' | '\r')
}

/// Maps one terminal key event to the shared semantic input command.
///
/// Mode-specific handlers intercept submission, cancellation, completion,
/// history navigation, and other contextual actions before using this map as
/// their editing fallback.
pub(crate) fn command_for_key(
    key: KeyEvent,
    capabilities: InputCapabilities,
) -> Option<InputCommand> {
    let command = match key.code {
        KeyCode::Enter | KeyCode::Char('\r' | '\n')
            if capabilities.multiline && should_insert_newline(key) =>
        {
            InputCommand::InsertNewline
        }
        KeyCode::Backspace if is_line_delete_backspace(key) => InputCommand::DeleteCurrentLine,
        KeyCode::Backspace if is_word_delete_backspace(key) => InputCommand::DeleteWordBackward,
        KeyCode::Backspace => InputCommand::DeleteBackward,
        KeyCode::Delete => InputCommand::DeleteForward,
        KeyCode::Left if key.modifiers.contains(event::KeyModifiers::SUPER) => {
            InputCommand::MoveLineStart
        }
        KeyCode::Left
            if key
                .modifiers
                .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT) =>
        {
            InputCommand::MoveWordLeft
        }
        KeyCode::Left => InputCommand::MoveLeft,
        KeyCode::Right if key.modifiers.contains(event::KeyModifiers::SUPER) => {
            InputCommand::MoveLineEnd
        }
        KeyCode::Right
            if key
                .modifiers
                .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT) =>
        {
            InputCommand::MoveWordRight
        }
        KeyCode::Right => InputCommand::MoveRight,
        KeyCode::Up => InputCommand::MoveUp,
        KeyCode::Down => InputCommand::MoveDown,
        KeyCode::Home => InputCommand::MoveHome,
        KeyCode::End => InputCommand::MoveEnd,
        KeyCode::Char('z' | 'Z')
            if key.modifiers == event::KeyModifiers::CONTROL | event::KeyModifiers::SHIFT =>
        {
            InputCommand::Redo
        }
        KeyCode::Char('z') if is_control_key(key) => InputCommand::Undo,
        KeyCode::Char('y') if is_control_key(key) => InputCommand::Redo,
        KeyCode::Char('a') if is_control_key(key) => InputCommand::MoveLineStart,
        KeyCode::Char('e') if is_control_key(key) => InputCommand::MoveLineEnd,
        KeyCode::Char('f') if is_control_key(key) => InputCommand::MoveRight,
        KeyCode::Char('b') if is_control_key(key) => InputCommand::MoveLeft,
        KeyCode::Char('p') if is_control_key(key) => InputCommand::MoveUp,
        KeyCode::Char('n') if is_control_key(key) => InputCommand::MoveDown,
        KeyCode::Char('d') if is_control_key(key) => InputCommand::DeleteForward,
        KeyCode::Char('k') if is_control_key(key) => InputCommand::DeleteToLineEnd,
        KeyCode::Char('u') if is_control_key(key) => InputCommand::DeleteCurrentLine,
        KeyCode::Char('w') if is_control_key(key) => InputCommand::DeleteWordBackward,
        KeyCode::Char('b') if is_alt_key(key) => InputCommand::MoveWordLeft,
        KeyCode::Char('f') if is_alt_key(key) => InputCommand::MoveWordRight,
        KeyCode::Char(character)
            if capabilities.multiline && is_control_newline_key(key, character) =>
        {
            InputCommand::InsertNewline
        }
        KeyCode::Char(character) if is_insertable_char_key(key) => InputCommand::Insert(character),
        _ => return None,
    };

    Some(command)
}

// ---------------------------------------------------------------------------
// Cursor position queries
// ---------------------------------------------------------------------------

/// Returns whether the input cursor is on the first line of text.
///
/// True when no newline characters appear before the cursor position,
/// including when the input is empty.
pub(crate) fn is_cursor_on_first_line(input: &InputState) -> bool {
    input.text().chars().take(input.cursor).all(|ch| ch != '\n')
}

/// Returns whether the input cursor is on the last line of text.
///
/// True when no newline characters appear after the cursor position,
/// including when the input is empty.
pub(crate) fn is_cursor_on_last_line(input: &InputState) -> bool {
    input.text().chars().skip(input.cursor).all(|ch| ch != '\n')
}

// ---------------------------------------------------------------------------
// Text normalization
// ---------------------------------------------------------------------------

/// Normalizes pasted text line endings to `\n`.
///
/// Replaces `\r\n` (Windows) and standalone `\r` (classic Mac) with `\n`.
pub(crate) fn normalize_pasted_text(pasted_text: &str) -> String {
    let mut normalized_text = String::with_capacity(pasted_text.len());
    let mut characters = pasted_text.chars().peekable();

    while let Some(character) = characters.next() {
        if character == '\r' {
            if matches!(characters.peek(), Some(&'\n')) {
                // Consume the trailing `\n` from a `\r\n` sequence.
                let _ = characters.next();
            }

            normalized_text.push('\n');

            continue;
        }

        normalized_text.push(character);
    }

    normalized_text
}

/// Normalizes pasted text and keeps only its first line for a single-line
/// input field.
pub(crate) fn normalize_single_line_pasted_text(pasted_text: &str) -> String {
    normalize_pasted_text(pasted_text)
        .split('\n')
        .next()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
