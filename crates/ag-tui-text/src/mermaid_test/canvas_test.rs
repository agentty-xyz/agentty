use crate::mermaid::{CONNECT_LEFT, CONNECT_RIGHT, Canvas};

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
