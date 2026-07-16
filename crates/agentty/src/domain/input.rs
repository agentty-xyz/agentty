use std::collections::VecDeque;

/// Maximum number of text snapshots retained by each input's undo or redo
/// stack.
pub(crate) const INPUT_HISTORY_LIMIT: usize = 100;

/// Semantic editing command shared by every text input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputCommand {
    /// Deletes the character immediately before the cursor.
    DeleteBackward,
    /// Deletes the current line and one adjacent newline separator.
    DeleteCurrentLine,
    /// Deletes the character at the cursor.
    DeleteForward,
    /// Deletes text from the cursor to the end of the current line.
    DeleteToLineEnd,
    /// Deletes the previous word and adjacent separator whitespace.
    DeleteWordBackward,
    /// Inserts one character at the cursor.
    Insert(char),
    /// Inserts a newline at the cursor.
    InsertNewline,
    /// Inserts text at the cursor.
    InsertText(String),
    /// Moves the cursor to the next line while preserving visual column.
    MoveDown,
    /// Moves the cursor to the end of the buffer.
    MoveEnd,
    /// Moves the cursor to the start of the buffer.
    MoveHome,
    /// Moves the cursor one character to the left.
    MoveLeft,
    /// Moves the cursor to the end of the current line.
    MoveLineEnd,
    /// Moves the cursor to the start of the current line.
    MoveLineStart,
    /// Moves the cursor one character to the right.
    MoveRight,
    /// Moves the cursor to the previous line while preserving visual column.
    MoveUp,
    /// Moves the cursor to the start of the previous word.
    MoveWordLeft,
    /// Moves the cursor to the start of the next word.
    MoveWordRight,
    /// Reapplies the most recently undone text mutation.
    Redo,
    /// Replaces one character-indexed range with new text.
    ReplaceRange {
        /// Inclusive character index at which replacement begins.
        start: usize,
        /// Exclusive character index at which replacement ends.
        end: usize,
        /// Text inserted in place of the selected range.
        text: String,
    },
    /// Restores the state before the most recent text mutation.
    Undo,
}

/// Observable result of applying one [`InputCommand`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEffect {
    /// The cursor moved without changing text.
    CursorMoved,
    /// The text buffer changed.
    TextChanged,
    /// Neither the cursor nor the text buffer changed.
    Unchanged,
}

/// Text and cursor snapshot stored by bounded undo/redo history.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InputSnapshot {
    cursor: usize,
    revision: u64,
    text: String,
}

/// Heap-backed history keeps `InputState` compact when it is embedded in
/// larger application-mode snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct InputHistory {
    next_revision: u64,
    redo_stack: VecDeque<InputSnapshot>,
    revision: u64,
    undo_stack: VecDeque<InputSnapshot>,
}

/// Editable text input with a character-based cursor index and bounded edit
/// history.
///
/// The derived `Default` produces an empty input with the cursor at position
/// `0` and an empty text buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputState {
    /// Cursor position measured in Unicode scalar values from the start.
    pub cursor: usize,
    history: Box<InputHistory>,
    text: String,
}

impl InputState {
    /// Creates an input state from existing text with the cursor at the end.
    pub fn with_text(text: String) -> Self {
        let cursor = text.chars().count();

        Self {
            cursor,
            history: Box::default(),
            text,
        }
    }

    /// Returns the current text buffer.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the stable identity of the current text snapshot.
    pub fn revision(&self) -> u64 {
        self.history.revision
    }

    /// Returns whether `revision` is the current snapshot or remains
    /// reachable through bounded undo/redo history.
    pub fn retains_revision(&self, revision: u64) -> bool {
        self.history.revision == revision
            || self
                .history
                .undo_stack
                .iter()
                .chain(&self.history.redo_stack)
                .any(|snapshot| snapshot.revision == revision)
    }

    /// Replaces the entire buffer with a fresh revision and clears edit
    /// history, preserving revision uniqueness for external state tracking.
    pub fn reset_text(&mut self, text: String) {
        self.cursor = text.chars().count();
        self.history.redo_stack.clear();
        self.history.undo_stack.clear();
        self.history.next_revision = self.history.next_revision.saturating_add(1);
        self.history.revision = self.history.next_revision;
        self.text = text;
    }

    /// Drains and returns the text buffer, then resets the cursor to `0`.
    pub fn take_text(&mut self) -> String {
        self.cursor = 0;
        self.history.redo_stack.clear();
        self.history.undo_stack.clear();
        self.history.next_revision = self.history.next_revision.saturating_add(1);
        self.history.revision = self.history.next_revision;

        std::mem::take(&mut self.text)
    }

    /// Returns whether the current text buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Inserts one character at the cursor and advances the cursor by one.
    pub fn insert_char(&mut self, ch: char) {
        let snapshot = self.snapshot();
        let byte_offset = self.byte_offset();
        self.text.insert(byte_offset, ch);
        self.cursor += 1;
        self.record_text_change(snapshot);
    }

    /// Inserts `text` at the cursor and moves the cursor to the end of the
    /// inserted content.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let snapshot = self.snapshot();
        let byte_offset = self.byte_offset();
        self.text.insert_str(byte_offset, text);
        self.cursor += text.chars().count();
        self.record_text_change(snapshot);
    }

    /// Inserts a newline at the cursor position.
    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Deletes the character immediately before the cursor.
    pub fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let snapshot = self.snapshot();
        let start = self.byte_offset_at(self.cursor - 1);
        let end = self.byte_offset();
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
        self.record_text_change(snapshot);
    }

    /// Deletes the entire current line including one adjacent newline
    /// separator.
    ///
    /// When the line has a preceding newline, removes it so the cursor
    /// lands at the end of the previous line. When the line is the first
    /// line and has a following newline, removes that instead so the
    /// cursor lands at position `0`. For a single-line buffer, clears
    /// all text.
    pub fn delete_current_line(&mut self) {
        let characters: Vec<char> = self.text.chars().collect();
        let cursor_pos = self.cursor.min(characters.len());

        let mut line_start = cursor_pos;
        while line_start > 0 && characters[line_start - 1] != '\n' {
            line_start -= 1;
        }

        let mut line_end = cursor_pos;
        while line_end < characters.len() && characters[line_end] != '\n' {
            line_end += 1;
        }

        let (delete_start, delete_end) = if line_start > 0 {
            (line_start - 1, line_end)
        } else if line_end < characters.len() {
            (line_start, line_end + 1)
        } else {
            (line_start, line_end)
        };

        self.replace_range(delete_start, delete_end, "");
    }

    /// Deletes the character at the cursor position.
    pub fn delete_forward(&mut self) {
        let char_count = self.text.chars().count();
        if self.cursor >= char_count {
            return;
        }

        let snapshot = self.snapshot();
        let start = self.byte_offset();
        let end = self.byte_offset_at(self.cursor + 1);
        self.text.replace_range(start..end, "");
        self.record_text_change(snapshot);
    }

    /// Deletes the previous word and adjacent separator whitespace.
    pub fn delete_word_backward(&mut self) {
        let Some((start, end)) = self.word_delete_range() else {
            return;
        };

        self.replace_range(start, end, "");
    }

    /// Moves the cursor one character to the left.
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor one character to the right.
    pub fn move_right(&mut self) {
        let char_count = self.text.chars().count();
        if self.cursor < char_count {
            self.cursor += 1;
        }
    }

    /// Moves the cursor to the previous line while preserving visual column.
    pub fn move_up(&mut self) {
        let (line, column) = self.line_column();
        if line == 0 {
            self.cursor = 0;

            return;
        }

        let mut current_line = 0;
        let mut line_start = 0;

        for (char_index, ch) in self.text.chars().enumerate() {
            if current_line == line - 1 {
                break;
            }
            if ch == '\n' {
                current_line += 1;
                line_start = char_index + 1;
            }
        }

        let prev_line_start = line_start;
        let prev_line_len = self
            .text
            .chars()
            .skip(prev_line_start)
            .take_while(|&c| c != '\n')
            .count();
        self.cursor = prev_line_start + column.min(prev_line_len);
    }

    /// Moves the cursor to the next line while preserving visual column.
    pub fn move_down(&mut self) {
        let (line, column) = self.line_column();
        let line_count = self.text.chars().filter(|&c| c == '\n').count() + 1;

        if line >= line_count - 1 {
            self.cursor = self.text.chars().count();

            return;
        }

        let mut char_index = 0;
        let mut current_line = 0;

        for ch in self.text.chars() {
            char_index += 1;
            if ch == '\n' {
                current_line += 1;
                if current_line == line + 1 {
                    break;
                }
            }
        }

        let next_line_start = char_index;
        let next_line_len = self
            .text
            .chars()
            .skip(next_line_start)
            .take_while(|&c| c != '\n')
            .count();
        self.cursor = next_line_start + column.min(next_line_len);
    }

    /// Moves the cursor to the start of the buffer.
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Moves the cursor to the end of the buffer.
    pub fn move_end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// Moves the cursor to the start of the previous word.
    pub fn move_word_left(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let characters: Vec<char> = self.text.chars().collect();
        let mut cursor = self.cursor;

        while cursor > 0 && characters[cursor - 1].is_whitespace() {
            cursor -= 1;
        }

        while cursor > 0 && !characters[cursor - 1].is_whitespace() {
            cursor -= 1;
        }

        self.cursor = cursor;
    }

    /// Moves the cursor to the start of the next word.
    pub fn move_word_right(&mut self) {
        let characters: Vec<char> = self.text.chars().collect();
        let mut cursor = self.cursor;

        while cursor < characters.len() && !characters[cursor].is_whitespace() {
            cursor += 1;
        }

        while cursor < characters.len() && characters[cursor].is_whitespace() {
            cursor += 1;
        }

        self.cursor = cursor;
    }

    /// Moves the cursor to the start of the current line.
    ///
    /// Scans backward from the cursor to the nearest preceding newline (or
    /// the beginning of the buffer) and places the cursor there.
    pub fn move_line_start(&mut self) {
        let characters: Vec<char> = self.text.chars().collect();
        let mut cursor = self.cursor;

        while cursor > 0 && characters[cursor - 1] != '\n' {
            cursor -= 1;
        }

        self.cursor = cursor;
    }

    /// Moves the cursor to the end of the current line.
    ///
    /// Scans forward from the cursor to the nearest following newline (or the
    /// end of the buffer) and places the cursor there.
    pub fn move_line_end(&mut self) {
        let characters: Vec<char> = self.text.chars().collect();
        let mut cursor = self.cursor;

        while cursor < characters.len() && characters[cursor] != '\n' {
            cursor += 1;
        }

        self.cursor = cursor;
    }

    /// Deletes all text from the cursor to the end of the current line.
    ///
    /// If the cursor is already at the end of a line (sitting on a newline
    /// character or at the buffer end), this is a no-op.
    pub fn delete_to_line_end(&mut self) {
        let characters: Vec<char> = self.text.chars().collect();
        let mut line_end = self.cursor;

        while line_end < characters.len() && characters[line_end] != '\n' {
            line_end += 1;
        }

        if line_end > self.cursor {
            let snapshot = self.snapshot();
            let start_byte = self.byte_offset();
            let end_byte = self.byte_offset_at(line_end);
            self.text.replace_range(start_byte..end_byte, "");
            self.record_text_change(snapshot);
        }
    }

    /// Returns the character range removed by [`Self::delete_to_line_end`].
    #[must_use]
    pub fn line_end_delete_range(&self) -> Option<(usize, usize)> {
        let characters: Vec<char> = self.text.chars().collect();
        let mut line_end = self.cursor;

        while line_end < characters.len() && characters[line_end] != '\n' {
            line_end += 1;
        }

        (line_end > self.cursor).then_some((self.cursor, line_end))
    }

    /// Returns the character range for deleting the previous word and its
    /// adjacent separator whitespace.
    #[must_use]
    pub fn word_delete_range(&self) -> Option<(usize, usize)> {
        if self.cursor == 0 {
            return None;
        }

        let characters: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;

        while start > 0 && characters[start - 1].is_whitespace() {
            start -= 1;
        }

        while start > 0 && !characters[start - 1].is_whitespace() {
            start -= 1;
        }

        while start > 0 && characters[start - 1].is_whitespace() {
            start -= 1;
        }

        Some((start, self.cursor))
    }

    /// Extracts the `@query` text at the current cursor position.
    ///
    /// Returns `Some((at_char_index, query))` if the cursor sits inside an
    /// `@query` token where `@` is preceded by whitespace or is at position 0.
    pub fn at_mention_query(&self) -> Option<(usize, String)> {
        extract_at_mention_query(&self.text, self.cursor)
    }

    /// Replaces characters in `[start_char..end_char)` with `replacement`
    /// and moves the cursor to the end of the inserted text.
    pub fn replace_range(&mut self, start_char: usize, end_char: usize, replacement: &str) {
        let snapshot = self.snapshot();
        let start_byte = self.byte_offset_at(start_char);
        let end_byte = self.byte_offset_at(end_char);
        self.text.replace_range(start_byte..end_byte, replacement);
        self.cursor = start_char + replacement.chars().count();
        self.record_text_change(snapshot);
    }

    /// Applies one shared semantic editing command.
    pub fn apply(&mut self, command: InputCommand) -> InputEffect {
        let cursor_before = self.cursor;
        let revision_before = self.history.revision;

        match command {
            InputCommand::DeleteBackward => self.delete_backward(),
            InputCommand::DeleteCurrentLine => self.delete_current_line(),
            InputCommand::DeleteForward => self.delete_forward(),
            InputCommand::DeleteToLineEnd => self.delete_to_line_end(),
            InputCommand::DeleteWordBackward => self.delete_word_backward(),
            InputCommand::Insert(character) => self.insert_char(character),
            InputCommand::InsertNewline => self.insert_newline(),
            InputCommand::InsertText(text) => self.insert_text(&text),
            InputCommand::MoveDown => self.move_down(),
            InputCommand::MoveEnd => self.move_end(),
            InputCommand::MoveHome => self.move_home(),
            InputCommand::MoveLeft => self.move_left(),
            InputCommand::MoveLineEnd => self.move_line_end(),
            InputCommand::MoveLineStart => self.move_line_start(),
            InputCommand::MoveRight => self.move_right(),
            InputCommand::MoveUp => self.move_up(),
            InputCommand::MoveWordLeft => self.move_word_left(),
            InputCommand::MoveWordRight => self.move_word_right(),
            InputCommand::Redo => self.redo(),
            InputCommand::ReplaceRange { start, end, text } => {
                self.replace_range(start, end, &text);
            }
            InputCommand::Undo => self.undo(),
        }

        if self.history.revision != revision_before {
            return InputEffect::TextChanged;
        }

        if self.cursor != cursor_before {
            return InputEffect::CursorMoved;
        }

        InputEffect::Unchanged
    }

    /// Restores the most recent text mutation and cursor position.
    pub fn undo(&mut self) {
        let Some(snapshot) = self.history.undo_stack.pop_back() else {
            return;
        };

        let current = self.snapshot();
        Self::push_bounded(&mut self.history.redo_stack, current);
        self.restore(snapshot);
    }

    /// Reapplies the most recently undone text mutation.
    pub fn redo(&mut self) {
        let Some(snapshot) = self.history.redo_stack.pop_back() else {
            return;
        };

        let current = self.snapshot();
        Self::push_bounded(&mut self.history.undo_stack, current);
        self.restore(snapshot);
    }

    fn byte_offset(&self) -> usize {
        self.byte_offset_at(self.cursor)
    }

    fn byte_offset_at(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(index, _)| index)
    }

    fn line_column(&self) -> (usize, usize) {
        let mut line = 0;
        let mut column = 0;

        for (index, ch) in self.text.chars().enumerate() {
            if index == self.cursor {
                break;
            }
            if ch == '\n' {
                line += 1;
                column = 0;
            } else {
                column += 1;
            }
        }

        (line, column)
    }

    fn snapshot(&self) -> InputSnapshot {
        InputSnapshot {
            cursor: self.cursor,
            revision: self.history.revision,
            text: self.text.clone(),
        }
    }

    fn record_text_change(&mut self, snapshot: InputSnapshot) {
        if self.text == snapshot.text {
            return;
        }

        Self::push_bounded(&mut self.history.undo_stack, snapshot);
        self.history.redo_stack.clear();
        self.history.next_revision = self.history.next_revision.saturating_add(1);
        self.history.revision = self.history.next_revision;
    }

    fn push_bounded(stack: &mut VecDeque<InputSnapshot>, snapshot: InputSnapshot) {
        if stack.len() == INPUT_HISTORY_LIMIT {
            let _ = stack.pop_front();
        }

        stack.push_back(snapshot);
    }

    fn restore(&mut self, snapshot: InputSnapshot) {
        self.cursor = snapshot.cursor;
        self.history.revision = snapshot.revision;
        self.text = snapshot.text;
    }
}

/// Extracts an `@query` pattern ending at `cursor` from `text`.
///
/// Returns `Some((at_char_index, query_string))` if the cursor sits inside
/// an `@query` token where `@` starts at a lookup boundary (position `0`,
/// preceded by whitespace, or preceded by an opening delimiter such as `(`).
/// Returns `None` if no active at-mention is detected.
pub fn extract_at_mention_query(text: &str, cursor: usize) -> Option<(usize, String)> {
    if cursor == 0 {
        return None;
    }

    let chars: Vec<char> = text.chars().collect();
    let mut scan = cursor;

    while scan > 0 {
        scan -= 1;
        let ch = *chars.get(scan)?;

        if ch == '@' {
            if is_at_mention_boundary(chars.get(scan.wrapping_sub(1)).copied()) {
                let query: String = chars[scan + 1..cursor].iter().collect();

                return Some((scan, query));
            }

            return None;
        }

        if ch.is_whitespace() {
            return None;
        }
    }

    None
}

/// Returns whether the character before `@` starts a file lookup token.
pub(crate) fn is_at_mention_boundary(previous_character: Option<char>) -> bool {
    previous_character.is_none_or(|ch| ch.is_whitespace() || is_at_mention_opening_delimiter(ch))
}

/// Returns whether `ch` can appear inside an active `@` lookup token.
pub(crate) fn is_at_mention_query_character(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-')
}

/// Returns whether `ch` is an opening delimiter that can precede `@`.
fn is_at_mention_opening_delimiter(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_text_at_end_updates_text_and_cursor() {
        // Arrange
        let mut state = InputState::with_text("hello".to_string());

        // Act
        state.insert_text(" world");

        // Assert
        assert_eq!(state.text(), "hello world");
        assert_eq!(state.cursor, "hello world".chars().count());
    }

    #[test]
    fn test_insert_text_in_middle_preserves_surrounding_content() {
        // Arrange
        let mut state = InputState::with_text("hllo".to_string());
        state.cursor = 1;

        // Act
        state.insert_text("e");

        // Assert
        assert_eq!(state.text(), "hello");
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn test_delete_current_line_clears_single_line_content() {
        // Arrange
        let mut state = InputState::with_text("hello world".to_string());
        state.cursor = "hello".chars().count();

        // Act
        state.delete_current_line();

        // Assert
        assert_eq!(state.text(), "");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn test_delete_current_line_removes_last_line_and_preceding_newline() {
        // Arrange
        let mut state = InputState::with_text("first line\nsecond line".to_string());
        state.cursor = "first line\nsecond".chars().count();

        // Act
        state.delete_current_line();

        // Assert
        assert_eq!(state.text(), "first line");
        assert_eq!(state.cursor, "first line".chars().count());
    }

    #[test]
    fn test_delete_current_line_removes_middle_line_and_preceding_newline() {
        // Arrange
        let mut state = InputState::with_text("first line\nsecond line\nthird line".to_string());
        state.cursor = "first line\nsecond".chars().count();

        // Act
        state.delete_current_line();

        // Assert
        assert_eq!(state.text(), "first line\nthird line");
        assert_eq!(state.cursor, "first line".chars().count());
    }

    #[test]
    fn test_delete_current_line_removes_first_line_and_following_newline() {
        // Arrange
        let mut state = InputState::with_text("first line\nsecond line".to_string());
        state.cursor = "first".chars().count();

        // Act
        state.delete_current_line();

        // Assert
        assert_eq!(state.text(), "second line");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn test_extract_at_mention_query_accepts_parenthesized_lookup() {
        // Arrange
        let text = "review (@src/main.rs)";
        let cursor = "review (@src/main.rs".chars().count();

        // Act
        let query = extract_at_mention_query(text, cursor);

        // Assert
        assert_eq!(query, Some((8, "src/main.rs".to_string())));
    }

    #[test]
    fn test_extract_at_mention_query_rejects_email_pattern() {
        // Arrange
        let text = "person@example.com";
        let cursor = text.chars().count();

        // Act
        let query = extract_at_mention_query(text, cursor);

        // Assert
        assert_eq!(query, None);
    }

    #[test]
    fn test_move_line_start_moves_to_beginning_of_current_line() {
        // Arrange
        let mut state = InputState::with_text("first\nsecond\nthird".to_string());
        state.cursor = "first\nseco".chars().count();

        // Act
        state.move_line_start();

        // Assert
        assert_eq!(state.cursor, "first\n".chars().count());
    }

    #[test]
    fn test_move_line_start_stays_at_buffer_start_on_first_line() {
        // Arrange
        let mut state = InputState::with_text("hello world".to_string());
        state.cursor = 5;

        // Act
        state.move_line_start();

        // Assert
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn test_move_line_end_moves_to_end_of_current_line() {
        // Arrange
        let mut state = InputState::with_text("first\nsecond\nthird".to_string());
        state.cursor = "first\nse".chars().count();

        // Act
        state.move_line_end();

        // Assert
        assert_eq!(state.cursor, "first\nsecond".chars().count());
    }

    #[test]
    fn test_move_line_end_moves_to_buffer_end_on_last_line() {
        // Arrange
        let mut state = InputState::with_text("first\nsecond".to_string());
        state.cursor = "first\nse".chars().count();

        // Act
        state.move_line_end();

        // Assert
        assert_eq!(state.cursor, "first\nsecond".chars().count());
    }

    #[test]
    fn test_delete_to_line_end_removes_text_after_cursor_on_current_line() {
        // Arrange
        let mut state = InputState::with_text("first\nsecond\nthird".to_string());
        state.cursor = "first\nse".chars().count();

        // Act
        state.delete_to_line_end();

        // Assert
        assert_eq!(state.text(), "first\nse\nthird");
        assert_eq!(state.cursor, "first\nse".chars().count());
    }

    #[test]
    fn test_delete_to_line_end_is_noop_at_newline() {
        // Arrange
        let mut state = InputState::with_text("first\nsecond".to_string());
        state.cursor = "first".chars().count();

        // Act
        state.delete_to_line_end();

        // Assert
        assert_eq!(state.text(), "first\nsecond");
        assert_eq!(state.cursor, "first".chars().count());
    }

    #[test]
    fn test_delete_to_line_end_clears_rest_of_single_line() {
        // Arrange
        let mut state = InputState::with_text("hello world".to_string());
        state.cursor = "hello".chars().count();

        // Act
        state.delete_to_line_end();

        // Assert
        assert_eq!(state.text(), "hello");
        assert_eq!(state.cursor, "hello".chars().count());
    }

    #[test]
    fn test_word_movement_and_deletion_share_input_state_behavior() {
        // Arrange
        let mut state = InputState::with_text("hello brave world".to_string());

        // Act
        state.move_word_left();
        let word_start = state.cursor;
        state.move_end();
        state.delete_word_backward();

        // Assert
        assert_eq!(word_start, "hello brave ".chars().count());
        assert_eq!(state.text(), "hello brave");
        assert_eq!(state.cursor, "hello brave".chars().count());
    }

    #[test]
    fn test_word_operations_handle_buffer_start_and_trailing_whitespace() {
        // Arrange
        let mut state = InputState::with_text("hello  ".to_string());

        // Act
        state.move_word_left();
        let word_start = state.cursor;
        state.move_home();
        state.move_word_left();
        state.delete_word_backward();

        // Assert
        assert_eq!(word_start, 0);
        assert_eq!(state.cursor, 0);
        assert_eq!(state.text(), "hello  ");
    }

    #[test]
    fn test_delete_ranges_report_line_end_and_whitespace_prefixed_word() {
        // Arrange
        let mut state = InputState::with_text("first line\nsecond word  ".to_string());
        state.cursor = "first ".chars().count();

        // Act
        let line_end_range = state.line_end_delete_range();
        state.move_end();
        let word_range = state.word_delete_range();
        state.move_home();
        let empty_word_range = state.word_delete_range();

        // Assert
        assert_eq!(line_end_range, Some((6, 10)));
        assert_eq!(word_range, Some((17, 24)));
        assert_eq!(empty_word_range, None);
    }

    #[test]
    fn test_apply_reports_effect_from_revision_and_cursor_changes() {
        // Arrange
        let mut state = InputState::with_text("hello".to_string());

        // Act
        let cursor_effect = state.apply(InputCommand::MoveLeft);
        let text_effect = state.apply(InputCommand::Insert('!'));
        let home_effect = state.apply(InputCommand::MoveHome);
        let end_effect = state.apply(InputCommand::MoveEnd);
        let unchanged_effect = state.apply(InputCommand::MoveRight);

        // Assert
        assert_eq!(cursor_effect, InputEffect::CursorMoved);
        assert_eq!(text_effect, InputEffect::TextChanged);
        assert_eq!(home_effect, InputEffect::CursorMoved);
        assert_eq!(end_effect, InputEffect::CursorMoved);
        assert_eq!(unchanged_effect, InputEffect::Unchanged);
    }

    #[test]
    fn test_noop_edit_and_empty_undo_leave_input_unchanged() {
        // Arrange
        let mut state = InputState::with_text("hello".to_string());
        let revision = state.revision();

        // Act
        state.replace_range(0, 0, "");
        state.undo();

        // Assert
        assert_eq!(state.text(), "hello");
        assert_eq!(state.revision(), revision);
    }

    #[test]
    fn test_undo_and_redo_restore_text_and_cursor() {
        // Arrange
        let mut state = InputState::with_text("helo".to_string());
        state.cursor = 3;
        state.insert_char('l');

        // Act
        state.undo();

        // Assert
        assert_eq!(state.text(), "helo");
        assert_eq!(state.cursor, 3);

        // Act
        state.redo();

        // Assert
        assert_eq!(state.text(), "hello");
        assert_eq!(state.cursor, 4);
    }

    #[test]
    fn test_undo_and_redo_restore_stable_revision_identity() {
        // Arrange
        let mut state = InputState::with_text("first".to_string());
        let first_revision = state.revision();
        state.insert_text(" second");
        let second_revision = state.revision();

        // Act
        state.undo();

        // Assert
        assert_eq!(state.revision(), first_revision);
        assert!(state.retains_revision(second_revision));

        // Act
        state.redo();

        // Assert
        assert_eq!(state.revision(), second_revision);
        assert!(state.retains_revision(first_revision));
    }

    #[test]
    fn test_new_edit_after_undo_clears_redo_history() {
        // Arrange
        let mut state = InputState::default();
        state.insert_text("first");
        state.undo();
        state.insert_text("second");

        // Act
        state.redo();

        // Assert
        assert_eq!(state.text(), "second");
    }

    #[test]
    fn test_pasted_text_is_one_undo_step() {
        // Arrange
        let mut state = InputState::with_text("prefix ".to_string());
        state.insert_text("pasted text");

        // Act
        state.undo();

        // Assert
        assert_eq!(state.text(), "prefix ");
        assert_eq!(state.cursor, "prefix ".chars().count());
    }
}
