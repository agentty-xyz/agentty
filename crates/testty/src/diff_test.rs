use crate::diff::{CellChange, FrameDiff};
use crate::frame::TerminalFrame;

#[test]
fn identical_frames_produce_no_changes() {
    // Arrange
    let data = b"Hello, World!";
    let frame_a = TerminalFrame::new(80, 24, data);
    let frame_b = TerminalFrame::new(80, 24, data);

    // Act
    let diff = FrameDiff::compute(&frame_a, &frame_b);

    // Assert
    assert!(diff.is_identical());
    assert!(diff.changed_regions().is_empty());
    assert_eq!(diff.summary(), [] as [std::string::String; 0]);
}

#[test]
fn single_cell_text_change_detected() {
    // Arrange
    let frame_a = TerminalFrame::new(80, 24, b"ABC");
    let frame_b = TerminalFrame::new(80, 24, b"AXC");

    // Act
    let diff = FrameDiff::compute(&frame_a, &frame_b);

    // Assert
    assert!(!diff.is_identical());
    assert_eq!(diff.cell_change(0, 0), Some(CellChange::Unchanged));
    assert_eq!(diff.cell_change(0, 1), Some(CellChange::TextChanged));
    assert_eq!(diff.cell_change(0, 2), Some(CellChange::Unchanged));
}

#[test]
fn adjacent_changes_merge_into_region() {
    // Arrange
    let frame_a = TerminalFrame::new(80, 24, b"AAAAAA");
    let frame_b = TerminalFrame::new(80, 24, b"ABBBBA");

    // Act
    let diff = FrameDiff::compute(&frame_a, &frame_b);
    let regions = diff.changed_regions();

    // Assert — four adjacent changed cells should merge.
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].region.col, 1);
    assert_eq!(regions[0].region.width, 4);
    assert_eq!(regions[0].change_type, CellChange::TextChanged);
}

#[test]
fn summary_formats_human_readable_text() {
    // Arrange
    let frame_a = TerminalFrame::new(80, 24, b"ABC");
    let frame_b = TerminalFrame::new(80, 24, b"AXC");

    // Act
    let diff = FrameDiff::compute(&frame_a, &frame_b);
    let summary = diff.summary();

    // Assert
    assert_eq!(summary.len(), 1);
    assert!(summary[0].contains("row 0"));
    assert!(summary[0].contains("col 1"));
    assert!(summary[0].contains("text changed"));
}

#[test]
fn style_change_detected() {
    // Arrange — same text but different style.
    let frame_a = TerminalFrame::new(80, 24, b"A");
    let frame_b = TerminalFrame::new(80, 24, b"\x1b[1mA\x1b[0m");

    // Act
    let diff = FrameDiff::compute(&frame_a, &frame_b);

    // Assert
    assert!(!diff.is_identical());
    assert_eq!(diff.cell_change(0, 0), Some(CellChange::StyleChanged));
}

#[test]
fn out_of_bounds_cell_returns_none() {
    // Arrange
    let frame = TerminalFrame::new(10, 5, b"Hi");
    let diff = FrameDiff::compute(&frame, &frame);

    // Act / Assert
    assert!(diff.cell_change(100, 100).is_none());
}

#[test]
fn different_size_frames_mark_extra_cells() {
    // Arrange — after frame is wider.
    let frame_a = TerminalFrame::new(5, 1, b"Hello");
    let frame_b = TerminalFrame::new(10, 1, b"Hello");

    // Act
    let diff = FrameDiff::compute(&frame_a, &frame_b);

    // Assert — first 5 cols unchanged, cols 5-9 are "out of bounds" =
    // changed.
    assert_eq!(diff.cell_change(0, 0), Some(CellChange::Unchanged));
    assert_eq!(diff.cell_change(0, 5), Some(CellChange::BothChanged));
}

#[test]
fn shrunk_frame_marks_removed_cols_as_changed() {
    // Arrange — after frame is narrower than before.
    let frame_a = TerminalFrame::new(10, 1, b"HelloWorld");
    let frame_b = TerminalFrame::new(5, 1, b"Hello");

    // Act
    let diff = FrameDiff::compute(&frame_a, &frame_b);

    // Assert — grid covers the wider frame; removed cols are changed.
    assert_eq!(diff.cols(), 10);
    assert_eq!(diff.cell_change(0, 0), Some(CellChange::Unchanged));
    assert_eq!(diff.cell_change(0, 4), Some(CellChange::Unchanged));
    assert_eq!(diff.cell_change(0, 5), Some(CellChange::BothChanged));
    assert_eq!(diff.cell_change(0, 9), Some(CellChange::BothChanged));
}

#[test]
fn shrunk_frame_marks_removed_rows_as_changed() {
    // Arrange — after frame has fewer rows.
    let frame_a = TerminalFrame::new(5, 3, b"A\nB\nC");
    let frame_b = TerminalFrame::new(5, 1, b"A");

    // Act
    let diff = FrameDiff::compute(&frame_a, &frame_b);

    // Assert — grid covers all 3 rows; removed rows are changed.
    assert_eq!(diff.rows(), 3);
    assert!(!diff.is_identical());
    assert_eq!(diff.cell_change(1, 0), Some(CellChange::BothChanged));
    assert_eq!(diff.cell_change(2, 0), Some(CellChange::BothChanged));
}

#[test]
fn summary_single_col_format() {
    // Arrange — change a single cell.
    let frame_a = TerminalFrame::new(80, 24, b"A");
    let frame_b = TerminalFrame::new(80, 24, b"B");

    // Act
    let diff = FrameDiff::compute(&frame_a, &frame_b);
    let summary = diff.summary();

    // Assert — single column should say "col X" not "cols X-X".
    assert_eq!(summary.len(), 1);
    assert!(summary[0].contains("col 0:"));
}

#[test]
fn adjacent_text_and_style_changes_form_a_mixed_region() {
    // Arrange
    let before = TerminalFrame::new(3, 1, b"abc");
    let after = TerminalFrame::new(3, 1, b"z\x1b[31mb\x1b[0mc");

    // Act
    let diff = FrameDiff::compute(&before, &after);
    let regions = diff.changed_regions();

    // Assert
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].change_type, CellChange::BothChanged);
    assert_eq!(regions[0].region.col, 0);
    assert_eq!(regions[0].region.width, 2);
}

#[test]
fn a_cell_can_change_text_and_style_together() {
    // Arrange
    let before = TerminalFrame::new(1, 1, b"x");
    let after = TerminalFrame::new(1, 1, b"\x1b[31my");

    // Act
    let diff = FrameDiff::compute(&before, &after);

    // Assert
    assert_eq!(diff.cell_change(0, 0), Some(CellChange::BothChanged));
    assert_eq!(diff.summary(), vec!["row 0, col 0: text and style changed"]);
}
