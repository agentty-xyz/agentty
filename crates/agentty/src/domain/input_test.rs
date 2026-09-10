use super::{InputCommand, InputEffect, InputState, extract_at_mention_query};

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
