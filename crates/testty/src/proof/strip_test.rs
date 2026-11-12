use super::{LABEL_BG, LABEL_FG, compose_strip, render_all_frames};
use crate::frame::TerminalFrame;
use crate::proof::backend::{ProofBackend, RenderContext};
use crate::proof::report::{ProofError, ProofReport};
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
fn label_skips_non_ascii_characters() {
    // Arrange — the label text is `Step 1: [é] ...`; `é` is character 9.
    let frame = TerminalFrame::new(20, 1, b"");
    let mut report = ProofReport::new("label_test");
    report.add_capture("é", "accent", &frame);

    // Act
    let rendered_frames = render_all_frames(&report);
    let strip = compose_strip(&rendered_frames, &report);

    // Assert — the `S` cell has text pixels; the `é` cell is blank label
    // background.
    let cell_pixels = |start_x: u32| {
        (8..24).flat_map(move |pixel_y| (start_x..start_x + 8).map(move |x| (x, pixel_y)))
    };
    assert!(cell_pixels(4).any(|(x, y)| *strip.get_pixel(x, y) == LABEL_FG));
    assert!(cell_pixels(4 + 9 * 8).all(|(x, y)| *strip.get_pixel(x, y) == LABEL_BG));
}

#[test]
fn strip_backend_reports_unwritable_output_as_format_error() {
    // Arrange
    let frame = TerminalFrame::new(20, 3, b"Hello strip");
    let mut report = ProofReport::new("png_test");
    report.add_capture("snap", "Snapshot", &frame);
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let output_path = temp_dir.path().join("missing").join("strip.png");

    // Act
    let result = ScreenshotStripBackend.render(&RenderContext::new(&report, &output_path));

    // Assert
    assert!(matches!(result, Err(ProofError::Format(_))));
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
