//! Mermaid canvas bounds, border-tee, and label-placement tests.

use crate::mermaid::{CONNECT_DOWN, CONNECT_LEFT, CONNECT_RIGHT, CONNECT_UP, Canvas};

#[test]
fn test_canvas_replaces_only_straight_border_cells_with_tees() {
    // Arrange
    let mut canvas = Canvas::new(4, 2);
    for column in 0..4 {
        canvas.put_border(column, 0, '─');
    }
    canvas.put_border(0, 1, '│');
    canvas.put_border(1, 1, '│');
    canvas.put_border(2, 1, '┌');
    canvas.put_label(3, 1, 'x');

    // Act
    canvas.put_border_tee(0, 0, CONNECT_DOWN);
    canvas.put_border_tee(1, 0, CONNECT_UP);
    canvas.put_border_tee(2, 0, CONNECT_LEFT);
    canvas.put_border_tee(0, 1, CONNECT_RIGHT);
    canvas.put_border_tee(1, 1, CONNECT_LEFT);
    canvas.put_border_tee(2, 1, CONNECT_DOWN);
    canvas.put_border_tee(3, 1, CONNECT_DOWN);
    canvas.put_border_tee(4, 0, CONNECT_DOWN);
    canvas.put_border_tee(0, 2, CONNECT_DOWN);
    let diagram = canvas.into_diagram();

    // Assert
    assert_eq!(diagram.lines[0].to_string(), "┬┴──");
    assert_eq!(diagram.lines[1].to_string(), "├┤┌x");
}

#[test]
fn test_canvas_writes_label_beside_segment_only_in_free_cells() {
    // Arrange
    let mut canvas = Canvas::new(8, 4);
    canvas.merge_connector(2, 0, CONNECT_UP | CONNECT_DOWN);
    canvas.merge_connector(4, 1, CONNECT_UP | CONNECT_DOWN);
    canvas.put_border(7, 1, '│');
    canvas.merge_connector(1, 2, CONNECT_UP | CONNECT_DOWN);
    canvas.put_border(5, 2, '│');
    canvas.merge_connector(3, 3, CONNECT_UP | CONNECT_DOWN);

    // Act
    let is_written_right = canvas.try_write_label_beside(2, 0, "ab");
    let is_written_left = canvas.try_write_label_beside(4, 1, "ab");
    let is_written_between = canvas.try_write_label_beside(1, 2, "ab");
    let is_written_too_long = canvas.try_write_label_beside(3, 3, "abcdef");
    let is_written_outside = canvas.try_write_label_beside(0, 4, "ab");
    let diagram = canvas.into_diagram();

    // Assert
    assert!(is_written_right);
    assert!(is_written_left);
    assert!(!is_written_between);
    assert!(!is_written_too_long);
    assert!(!is_written_outside);
    assert_eq!(diagram.lines[0].to_string(), "  │ ab");
    assert_eq!(diagram.lines[1].to_string(), " ab │  │");
    assert_eq!(diagram.lines[2].to_string(), " │   │");
    assert_eq!(diagram.lines[3].to_string(), "   │");
}

#[test]
fn test_canvas_ignores_out_of_bounds_writes() {
    // Arrange
    let mut canvas = Canvas::new(2, 1);

    // Act
    canvas.merge_connector(2, 0, CONNECT_LEFT | CONNECT_RIGHT);
    canvas.merge_connector(0, 1, CONNECT_LEFT | CONNECT_RIGHT);
    canvas.put_label(2, 0, 'x');
    canvas.try_put_marker(0, 1, Some('1'));
    canvas.try_write_label(0, 1, "a");
    canvas.try_write_label(1, 0, "ab");
    canvas.put_label(1, 0, 'y');
    let diagram = canvas.into_diagram();

    // Assert
    assert_eq!(diagram.lines.len(), 1);
    assert_eq!(diagram.lines[0].to_string(), " y");
    assert_eq!(diagram.width, 2);
}

#[test]
fn test_canvas_writes_label_only_over_empty_or_horizontal_cells() {
    // Arrange
    let mut canvas = Canvas::new(3, 2);
    canvas.put_border(0, 0, '│');
    canvas.merge_connector(0, 1, CONNECT_LEFT | CONNECT_RIGHT);

    // Act
    canvas.try_write_label(0, 0, "ab");
    canvas.try_write_label(0, 1, "cd");
    let diagram = canvas.into_diagram();

    // Assert
    assert_eq!(diagram.lines[0].to_string(), "│");
    assert_eq!(diagram.lines[1].to_string(), "cd");
}
