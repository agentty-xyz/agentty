use super::{compose_strip, render_all_frames};
use crate::frame::TerminalFrame;
use crate::proof::backend::{ProofBackend, RenderContext};
use crate::proof::report::ProofReport;
use crate::proof::strip::ScreenshotStripBackend;

#[test]
fn strip_of_two_captures_is_taller_than_single_frame() {
    // Arrange
    let frame_a = TerminalFrame::new(40, 5, b"First");
    let frame_b = TerminalFrame::new(40, 5, b"Second");
    let mut report = ProofReport::new("strip_test");
    report.add_capture("first", "First capture", &frame_a);
    report.add_capture("second", "Second capture", &frame_b);

    // Act
    let rendered_frames = render_all_frames(&report);
    let strip = compose_strip(&rendered_frames, &report);

    // Assert — strip should be taller than a single frame (5*16 = 80px).
    let single_frame_height = 5 * 16;
    assert!(strip.height() > single_frame_height);
}

#[test]
fn strip_backend_writes_valid_png() {
    // Arrange
    let frame = TerminalFrame::new(20, 3, b"Hello strip");
    let mut report = ProofReport::new("png_test");
    report.add_capture("snap", "Snapshot", &frame);

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let output_path = temp_dir.path().join("strip.png");

    // Act
    let backend = ScreenshotStripBackend;
    backend
        .render(&RenderContext::new(&report, &output_path))
        .expect("render should succeed");

    // Assert — file exists and is a valid PNG.
    assert!(output_path.exists());
    let loaded = image::open(&output_path).expect("should open as image");
    assert!(loaded.width() > 0);
    assert!(loaded.height() > 0);
}

#[test]
fn empty_report_produces_minimal_strip() {
    // Arrange
    let report = ProofReport::new("empty");

    // Act
    let rendered_frames = render_all_frames(&report);
    let strip = compose_strip(&rendered_frames, &report);

    // Assert — minimal 1x1 image for empty reports.
    assert_eq!(strip.width(), 1);
    assert_eq!(strip.height(), 1);
}

#[test]
fn strip_width_matches_widest_frame() {
    // Arrange — two frames with different widths.
    let narrow = TerminalFrame::new(20, 3, b"Narrow");
    let wide = TerminalFrame::new(40, 3, b"Wide");
    let mut report = ProofReport::new("width_test");
    report.add_capture("narrow", "Narrow frame", &narrow);
    report.add_capture("wide", "Wide frame", &wide);

    // Act
    let rendered_frames = render_all_frames(&report);
    let strip = compose_strip(&rendered_frames, &report);

    // Assert — strip width should be 40 * 8 = 320 (widest frame).
    assert_eq!(strip.width(), 320);
}
