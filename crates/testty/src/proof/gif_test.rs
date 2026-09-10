use super::{DEFAULT_FRAME_DELAY_MS, MAX_FRAME_DELAY_MS, MIN_FRAME_DELAY_MS};
use crate::frame::TerminalFrame;
use crate::proof::backend::{ProofBackend, RenderContext};
use crate::proof::gif::GifBackend;
use crate::proof::report::ProofReport;

#[test]
fn gif_backend_default_delay() {
    // Arrange / Act
    let backend = GifBackend::default();

    // Assert
    assert_eq!(backend.frame_delay_ms(), DEFAULT_FRAME_DELAY_MS);
}

#[test]
fn gif_backend_custom_delay_clamped() {
    // Arrange / Act / Assert — below minimum.
    let too_low = GifBackend::with_delay_ms(50);
    assert_eq!(too_low.frame_delay_ms(), MIN_FRAME_DELAY_MS);

    // Above maximum.
    let too_high = GifBackend::with_delay_ms(10000);
    assert_eq!(too_high.frame_delay_ms(), MAX_FRAME_DELAY_MS);

    // Within range.
    let normal = GifBackend::with_delay_ms(1000);
    assert_eq!(normal.frame_delay_ms(), 1000);
}

#[test]
fn gif_backend_writes_valid_gif() {
    // Arrange
    let frame_a = TerminalFrame::new(20, 3, b"Frame 1");
    let frame_b = TerminalFrame::new(20, 3, b"Frame 2");
    let mut report = ProofReport::new("gif_test");
    report.add_capture("first", "First frame", &frame_a);
    report.add_capture("second", "Second frame", &frame_b);

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let output_path = temp_dir.path().join("proof.gif");

    // Act
    let backend = GifBackend::default();
    backend
        .render(&RenderContext::new(&report, &output_path))
        .expect("render should succeed");

    // Assert — file exists and has non-trivial size.
    assert!(output_path.exists());
    let metadata = std::fs::metadata(&output_path).expect("failed to read metadata");
    assert!(metadata.len() > 100, "GIF should have meaningful content");
}

#[test]
fn gif_backend_errors_on_empty_report() {
    // Arrange
    let report = ProofReport::new("empty");
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let output_path = temp_dir.path().join("empty.gif");

    // Act
    let backend = GifBackend::default();
    let result = backend.render(&RenderContext::new(&report, &output_path));

    // Assert
    assert!(result.is_err());
}

#[test]
fn gif_backend_single_frame_succeeds() {
    // Arrange
    let frame = TerminalFrame::new(10, 2, b"Single");
    let mut report = ProofReport::new("single");
    report.add_capture("only", "Only frame", &frame);

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let output_path = temp_dir.path().join("single.gif");

    // Act
    let backend = GifBackend::with_delay_ms(500);
    let result = backend.render(&RenderContext::new(&report, &output_path));

    // Assert
    assert!(result.is_ok());
    assert!(output_path.exists());
}
