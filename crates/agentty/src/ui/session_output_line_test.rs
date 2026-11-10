use std::sync::Arc;

use ratatui::text::Line;

use super::OutputLines;

#[test]
fn test_segments_preserve_rows_when_appending_trimming_and_scrolling() {
    // Arrange
    let shared: Arc<[Line<'static>]> =
        Arc::from([Line::from("one"), Line::from(""), Line::from("")]);
    let mut lines = OutputLines::default();

    // Act, Assert
    lines.pop();
    lines.extend([]);
    assert!(lines.is_empty());
    assert!(lines.last().is_none());
    lines.append(Arc::clone(&shared));
    lines.push(Line::from(""));
    assert_eq!(lines.trimmed_len(), 1);
    lines.pop();
    lines.pop();
    lines.extend([Line::from("two"), Line::from("three")]);
    assert_eq!(lines.len(), 4);
    assert_eq!(lines.trimmed_len(), 4);
    assert_eq!(lines.last().map(ToString::to_string), Some("three".into()));
    assert!(Arc::ptr_eq(&lines.segments[0].lines, &shared));
    let mut copied = OutputLines::default();
    copied.append_shared(&lines);
    assert!(Arc::ptr_eq(&copied.segments[0].lines, &shared));
    assert_eq!(
        copied.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["one", "", "two", "three"]
    );
    for (first, height, expected) in [
        (0, 4, vec!["one", "", "two", "three"]),
        (1, 2, vec!["", "two"]),
        (2, 1, vec!["two"]),
        (3, 10, vec!["three"]),
        (4, 2, vec![]),
        (usize::MAX, 2, vec![]),
        (0, 0, vec![]),
    ] {
        assert_eq!(
            lines
                .visible_lines(first, height)
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            expected
        );
    }
}
