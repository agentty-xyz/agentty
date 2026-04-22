/// Editable text input with a character-based cursor index.
///
/// The derived `Default` produces an empty input with the cursor at position
/// `0` and an empty text buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputState {
    /// Cursor position measured in Unicode scalar values from the start.
    pub cursor: usize,
    text: String,
}

impl InputState {
    /// Creates an input state from existing text with the cursor at the end.
    pub fn with_text(text: String) -> Self {
        let cursor = text.chars().count();

        Self { cursor, text }
    }

    /// Returns the current text buffer.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Drains and returns the text buffer, then resets the cursor to `0`.
    pub fn take_text(&mut self) -> String {
        self.cursor = 0;

        std::mem::take(&mut self.text)
    }

    /// Returns whether the current text buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Inserts one character at the cursor and advances the cursor by one.
    pub fn insert_char(&mut self, ch: char) {
        let byte_offset = self.byte_offset();
        self.text.insert(byte_offset, ch);
        self.cursor += 1;
    }

    /// Inserts `text` at the cursor and moves the cursor to the end of the
    /// inserted content.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let byte_offset = self.byte_offset();
        self.text.insert_str(byte_offset, text);
        self.cursor += text.chars().count();
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

        let start = self.byte_offset_at(self.cursor - 1);
        let end = self.byte_offset();
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
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

        let start = self.byte_offset();
        let end = self.byte_offset_at(self.cursor + 1);
        self.text.replace_range(start..end, "");
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
            let start_byte = self.byte_offset();
            let end_byte = self.byte_offset_at(line_end);
            self.text.replace_range(start_byte..end_byte, "");
        }
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
        let start_byte = self.byte_offset_at(start_char);
        let end_byte = self.byte_offset_at(end_char);
        self.text.replace_range(start_byte..end_byte, replacement);
        self.cursor = start_char + replacement.chars().count();
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
}
