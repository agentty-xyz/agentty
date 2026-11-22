//! Mouse selection, highlight, copy text, and link click tests.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::{ScreenAction, ScreenSelection, link_at_offset, trim_link};

/// Builds one mouse report at a screen cell.
fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        column,
        kind,
        modifiers: KeyModifiers::NONE,
        row,
    }
}

/// Builds a left-button press at a screen cell.
fn press(column: u16, row: u16) -> MouseEvent {
    mouse(MouseEventKind::Down(MouseButton::Left), column, row)
}

/// Builds a left-button drag report at a screen cell.
fn drag(column: u16, row: u16) -> MouseEvent {
    mouse(MouseEventKind::Drag(MouseButton::Left), column, row)
}

/// Builds a left-button release at a screen cell.
fn release(column: u16, row: u16) -> MouseEvent {
    mouse(MouseEventKind::Up(MouseButton::Left), column, row)
}

/// Returns the cells of `buffer` drawn with reversed colors.
fn reversed_cells(buffer: &Buffer) -> Vec<(u16, u16)> {
    let area = buffer.area;

    (area.top()..area.bottom())
        .flat_map(|row| (area.left()..area.right()).map(move |column| (column, row)))
        .filter(|position| buffer[*position].modifier.contains(Modifier::REVERSED))
        .collect()
}

/// Draws `buffer`, runs a press, drag, and release over it, and returns the
/// action handed out with the next frame.
fn drag_selection(buffer: &mut Buffer, from: (u16, u16), to: (u16, u16)) -> Option<ScreenAction> {
    let selection = ScreenSelection::default();
    selection.apply(buffer);
    selection.handle_mouse(press(from.0, from.1));
    selection.handle_mouse(drag(to.0, to.1));
    selection.handle_mouse(release(to.0, to.1));

    selection.apply(buffer).pop()
}

/// Draws `buffer`, clicks one cell of it, and returns the action handed out
/// with the next frame.
fn click(buffer: &mut Buffer, column: u16, row: u16) -> Option<ScreenAction> {
    let selection = ScreenSelection::default();
    selection.apply(buffer);
    selection.handle_mouse(press(column, row));
    selection.handle_mouse(release(column, row));

    selection.apply(buffer).pop()
}

#[test]
fn a_release_is_resolved_against_the_frame_drawn_before_it() {
    // Arrange
    let mut seen = Buffer::with_lines(["https://seen.example ", "seen text            "]);
    let mut scrolled = Buffer::with_lines(["https://other.example", "other text           "]);
    let selection = ScreenSelection::default();
    selection.apply(&mut seen);

    // Act
    selection.handle_mouse(press(3, 0));
    selection.handle_mouse(release(3, 0));
    selection.handle_mouse(press(0, 1));
    selection.handle_mouse(release(3, 1));
    let actions = selection.apply(&mut scrolled);

    // Assert
    assert_eq!(
        actions,
        vec![
            ScreenAction::OpenLink("https://seen.example".to_string()),
            ScreenAction::Copy("seen".to_string()),
        ]
    );
}

#[test]
fn a_release_before_the_first_frame_does_nothing() {
    // Arrange
    let mut buffer = Buffer::with_lines(["https://example.com"]);
    let selection = ScreenSelection::default();

    // Act
    selection.handle_mouse(press(3, 0));
    selection.handle_mouse(release(3, 0));
    selection.handle_mouse(press(0, 0));
    selection.handle_mouse(release(4, 0));
    let actions = selection.apply(&mut buffer);

    // Assert
    assert_eq!(actions, Vec::new());
}

#[test]
fn gestures_finished_between_two_frames_are_all_resolved_in_order() {
    // Arrange
    let mut buffer = Buffer::with_lines(["ab https://example.com"]);
    let selection = ScreenSelection::default();
    selection.apply(&mut buffer);
    selection.handle_mouse(press(0, 0));
    selection.handle_mouse(drag(1, 0));
    selection.handle_mouse(release(1, 0));
    selection.handle_mouse(press(4, 0));
    selection.handle_mouse(release(4, 0));
    selection.handle_mouse(press(0, 0));
    selection.handle_mouse(drag(1, 0));

    // Act
    let actions = selection.apply(&mut buffer);

    // Assert
    assert_eq!(
        actions,
        vec![
            ScreenAction::Copy("ab".to_string()),
            ScreenAction::OpenLink("https://example.com".to_string()),
        ]
    );
    assert_eq!(reversed_cells(&buffer), vec![(0, 0), (1, 0)]);
}

#[test]
fn dragging_highlights_the_selection_in_reading_order() {
    // Arrange
    let mut buffer = Buffer::with_lines(["abcd", "efgh", "ijkl"]);
    let selection = ScreenSelection::default();

    // Act
    let is_press_handled = selection.handle_mouse(press(2, 0));
    let is_drag_handled = selection.handle_mouse(drag(1, 2));
    let action = selection.apply(&mut buffer).pop();

    // Assert
    assert!(is_press_handled);
    assert!(is_drag_handled);
    assert_eq!(action, None);
    assert_eq!(
        reversed_cells(&buffer),
        vec![
            (2, 0),
            (3, 0),
            (0, 1),
            (1, 1),
            (2, 1),
            (3, 1),
            (0, 2),
            (1, 2)
        ]
    );
}

#[test]
fn highlighting_a_reversed_cell_restores_normal_colors() {
    // Arrange
    let mut buffer = Buffer::with_lines(["ab"]);
    buffer[(0, 0)].modifier.insert(Modifier::REVERSED);
    let selection = ScreenSelection::default();
    selection.handle_mouse(press(0, 0));
    selection.handle_mouse(drag(1, 0));

    // Act
    let actions = selection.apply(&mut buffer);

    // Assert
    assert_eq!(actions, Vec::new());
    assert_eq!(reversed_cells(&buffer), vec![(1, 0)]);
}

#[test]
fn pressing_without_dragging_highlights_nothing() {
    // Arrange
    let mut buffer = Buffer::with_lines(["abcd"]);
    let selection = ScreenSelection::default();
    selection.handle_mouse(press(1, 0));

    // Act
    let action = selection.apply(&mut buffer).pop();

    // Assert
    assert_eq!(action, None);
    assert_eq!(reversed_cells(&buffer), Vec::new());
}

#[test]
fn releasing_a_drag_copies_rows_without_trailing_blanks() {
    // Arrange
    let mut buffer = Buffer::with_lines(["first line   ", "second       ", "             "]);

    // Act
    let action = drag_selection(&mut buffer, (6, 0), (12, 2));

    // Assert
    assert_eq!(action, Some(ScreenAction::Copy("line\nsecond".to_string())));
    assert_eq!(reversed_cells(&buffer), Vec::new());
}

#[test]
fn dragging_backwards_copies_the_same_text() {
    // Arrange
    let mut buffer = Buffer::with_lines(["abcd", "efgh"]);

    // Act
    let action = drag_selection(&mut buffer, (1, 1), (2, 0));

    // Assert
    assert_eq!(action, Some(ScreenAction::Copy("cd\nef".to_string())));
}

#[test]
fn copied_text_keeps_one_copy_of_a_wide_symbol() {
    // Arrange
    let mut buffer = Buffer::with_lines(["a日本b"]);

    // Act
    let action = drag_selection(&mut buffer, (0, 0), (5, 0));

    // Assert
    assert_eq!(action, Some(ScreenAction::Copy("a日本b".to_string())));
}

#[test]
fn releasing_over_blank_cells_copies_nothing() {
    // Arrange
    let mut buffer = Buffer::with_lines(["ab    "]);

    // Act
    let action = drag_selection(&mut buffer, (3, 0), (5, 0));

    // Assert
    assert_eq!(action, None);
}

#[test]
fn a_resolved_gesture_is_not_resolved_twice() {
    // Arrange
    let mut buffer = Buffer::with_lines(["abcd"]);
    let selection = ScreenSelection::default();
    selection.apply(&mut buffer);
    selection.handle_mouse(press(0, 0));
    selection.handle_mouse(drag(1, 0));
    selection.handle_mouse(release(1, 0));

    // Act
    let first = selection.apply(&mut buffer).pop();
    let second = selection.apply(&mut buffer).pop();

    // Assert
    assert_eq!(first, Some(ScreenAction::Copy("ab".to_string())));
    assert_eq!(second, None);
}

#[test]
fn positions_outside_the_screen_are_clamped() {
    // Arrange
    let mut buffer = Buffer::with_lines(["abcd", "efgh"]);

    // Act
    let action = drag_selection(&mut buffer, (2, 0), (40, 9));

    // Assert
    assert_eq!(action, Some(ScreenAction::Copy("cd\nefgh".to_string())));
}

#[test]
fn an_empty_screen_selects_nothing() {
    // Arrange
    let mut buffer = Buffer::empty(Rect::new(3, 3, 0, 0));
    let selection = ScreenSelection::default();
    selection.handle_mouse(press(0, 0));
    selection.handle_mouse(drag(4, 4));

    // Act
    let highlight_action = selection.apply(&mut buffer).pop();
    selection.handle_mouse(release(4, 4));
    let release_action = selection.apply(&mut buffer).pop();

    // Assert
    assert_eq!(highlight_action, None);
    assert_eq!(release_action, None);
}

#[test]
fn reports_without_a_pressed_left_button_are_ignored() {
    // Arrange
    let mut buffer = Buffer::with_lines(["abcd"]);
    let selection = ScreenSelection::default();

    // Act
    let handled = [
        selection.handle_mouse(drag(1, 0)),
        selection.handle_mouse(release(1, 0)),
        selection.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), 1, 0)),
        selection.handle_mouse(mouse(MouseEventKind::Moved, 1, 0)),
    ];
    let action = selection.apply(&mut buffer).pop();

    // Assert
    assert_eq!(handled, [false; 4]);
    assert_eq!(action, None);
}

#[test]
fn cancel_drops_only_a_selection_whose_button_is_down() {
    // Arrange
    let mut buffer = Buffer::with_lines(["abcd"]);
    let selection = ScreenSelection::default();

    // Act
    let is_idle_cancelled = selection.cancel();
    selection.handle_mouse(press(0, 0));
    selection.handle_mouse(drag(2, 0));
    let is_selecting_cancelled = selection.cancel();
    let cancelled_action = selection.apply(&mut buffer).pop();
    selection.handle_mouse(press(0, 0));
    selection.handle_mouse(drag(2, 0));
    selection.handle_mouse(release(2, 0));
    let is_released_cancelled = selection.cancel();
    let released_action = selection.apply(&mut buffer).pop();

    // Assert
    assert!(!is_idle_cancelled);
    assert!(is_selecting_cancelled);
    assert_eq!(cancelled_action, None);
    assert_eq!(reversed_cells(&buffer), Vec::new());
    assert!(!is_released_cancelled);
    assert_eq!(released_action, Some(ScreenAction::Copy("abc".to_string())));
}

#[test]
fn clicking_a_link_opens_it() {
    // Arrange
    let mut buffer = Buffer::with_lines(["│ see https://example.com/a?b=1. │"]);

    // Act
    let first_cell = click(&mut buffer, 6, 0);
    let last_cell = click(&mut buffer, 29, 0);

    // Assert
    let expected = Some(ScreenAction::OpenLink(
        "https://example.com/a?b=1".to_string(),
    ));
    assert_eq!(first_cell, expected);
    assert_eq!(last_cell, expected);
}

#[test]
fn clicking_beside_a_link_does_nothing() {
    // Arrange
    let mut buffer = Buffer::with_lines(["see http://example.com. ok"]);

    // Act
    let before = click(&mut buffer, 3, 0);
    let trailing_dot = click(&mut buffer, 22, 0);
    let outside = click(&mut buffer, 90, 9);

    // Assert
    assert_eq!(before, None);
    assert_eq!(trailing_dot, None);
    assert_eq!(outside, None);
}

#[test]
fn clicking_a_link_after_a_wide_symbol_finds_its_cell() {
    // Arrange
    let mut buffer = Buffer::with_lines(["日本 https://example.com"]);

    // Act
    let action = click(&mut buffer, 5, 0);
    let wide_symbol = click(&mut buffer, 1, 0);

    // Assert
    assert_eq!(
        action,
        Some(ScreenAction::OpenLink("https://example.com".to_string()))
    );
    assert_eq!(wide_symbol, None);
}

#[test]
fn link_at_offset_picks_the_link_under_the_offset() {
    // Arrange
    let text = "a https://one.example b http://two.example c";

    // Act
    let first = link_at_offset(text, 2);
    let second = link_at_offset(text, 30);
    let between = link_at_offset(text, 22);
    let bare_scheme = link_at_offset("go to https:// now", 8);

    // Assert
    assert_eq!(first.as_deref(), Some("https://one.example"));
    assert_eq!(second.as_deref(), Some("http://two.example"));
    assert_eq!(between, None);
    assert_eq!(bare_scheme, None);
}

#[test]
fn trim_link_removes_prose_punctuation_but_keeps_balanced_parentheses() {
    // Arrange
    let cases = [
        ("https://example.com/a.", "https://example.com/a"),
        ("https://example.com/a).", "https://example.com/a"),
        ("https://example.com/a_(b)", "https://example.com/a_(b)"),
        ("https://example.com/a_(b)),", "https://example.com/a_(b)"),
        ("https://example.com/a]**", "https://example.com/a"),
        ("...", ""),
    ];

    for (link, expected) in cases {
        // Act
        let trimmed = trim_link(link);

        // Assert
        assert_eq!(trimmed, expected, "{link}");
    }
}

#[test]
fn link_characters_end_at_borders_and_quotes() {
    // Arrange
    let mut buffer = Buffer::with_lines(["\"https://example.com/x\"|https://b.example<"]);

    // Act
    let quoted = click(&mut buffer, 3, 0);
    let piped = click(&mut buffer, 26, 0);

    // Assert
    assert_eq!(
        quoted,
        Some(ScreenAction::OpenLink("https://example.com/x".to_string()))
    );
    assert_eq!(
        piped,
        Some(ScreenAction::OpenLink("https://b.example".to_string()))
    );
}

#[test]
fn releasing_at_another_cell_without_a_drag_report_still_selects() {
    // Arrange
    let selection = ScreenSelection::default();
    let mut buffer = Buffer::with_lines(["abcd"]);
    selection.apply(&mut buffer);
    selection.handle_mouse(press(0, 0));

    // Act
    selection.handle_mouse(release(3, 0));
    let action = selection.apply(&mut buffer).pop();

    // Assert
    assert_eq!(action, Some(ScreenAction::Copy("abcd".to_string())));
}
