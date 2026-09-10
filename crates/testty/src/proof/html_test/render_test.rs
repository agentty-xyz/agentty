use super::super::build_html;
use crate::frame::TerminalFrame;
use crate::proof::backend::{ProofBackend, RenderContext};
use crate::proof::html::{HtmlBackend, escape_html};
use crate::proof::report::ProofReport;

#[test]
fn html_contains_step_cards() {
    // Arrange
    let frame = TerminalFrame::new(20, 3, b"Hello HTML");
    let mut report = ProofReport::new("html_test");
    report.add_capture("init", "Initial state", &frame);
    report.add_capture("done", "Final state", &frame);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert
    assert!(html.contains("Step 1"));
    assert!(html.contains("Step 2"));
    assert!(html.contains("[init]"));
    assert!(html.contains("[done]"));
    assert!(html.contains("Initial state"));
    assert!(html.contains("Final state"));
}

#[test]
fn html_contains_embedded_images() {
    // Arrange
    let frame = TerminalFrame::new(10, 2, b"Img");
    let mut report = ProofReport::new("image_test");
    report.add_capture("snap", "Snapshot", &frame);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert
    assert!(html.contains("data:image/png;base64,"));
}

#[test]
fn html_contains_assertion_results() {
    // Arrange
    let frame = TerminalFrame::new(20, 3, b"Test");
    let mut report = ProofReport::new("assert_html");
    report.add_capture("check", "Check", &frame);
    report.add_assertion("check", true, "text visible");
    report.add_assertion("check", false, "color match");

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert
    assert!(html.contains("class=\"assertion pass\""));
    assert!(html.contains("class=\"assertion fail\""));
    assert!(html.contains("text visible"));
    assert!(html.contains("color match"));
}

#[test]
fn html_contains_diff_summaries() {
    // Arrange
    let frame_a = TerminalFrame::new(20, 3, b"Before");
    let frame_b = TerminalFrame::new(20, 3, b"After!");
    let mut report = ProofReport::new("diff_html");
    report.add_capture("before", "Before", &frame_a);
    report.add_capture("after", "After", &frame_b);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert
    assert!(html.contains("Changes from previous step"));
}

#[test]
fn html_is_self_contained() {
    // Arrange
    let frame = TerminalFrame::new(10, 2, b"SC");
    let mut report = ProofReport::new("self_contained");
    report.add_capture("snap", "Snapshot", &frame);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — has doctype, head with style, body, and closing tags.
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("<style>"));
    assert!(html.contains("</html>"));
}

#[test]
fn html_backend_writes_file() {
    // Arrange
    let frame = TerminalFrame::new(10, 2, b"File");
    let mut report = ProofReport::new("file_test");
    report.add_capture("snap", "Snapshot", &frame);

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let output_path = temp_dir.path().join("report.html");

    // Act
    let backend = HtmlBackend;
    backend
        .render(&RenderContext::new(&report, &output_path))
        .expect("render should succeed");

    // Assert
    assert!(output_path.exists());
    let content = std::fs::read_to_string(&output_path).expect("failed to read");
    assert!(content.contains("Proof Report: file_test"));
}

#[test]
fn escape_html_handles_special_chars() {
    // Arrange / Act / Assert
    assert_eq!(escape_html("<script>"), "&lt;script&gt;");
    assert_eq!(escape_html("a&b"), "a&amp;b");
    assert_eq!(escape_html("\"quoted\""), "&quot;quoted&quot;");
}

#[test]
fn html_escapes_scenario_name_in_header() {
    // Arrange
    let frame = TerminalFrame::new(10, 2, b"X");
    let mut report = ProofReport::new("<script>alert('xss')</script>");
    report.add_capture("snap", "Snapshot", &frame);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — raw script tag must not appear.
    assert!(!html.contains("<script>alert"));
    assert!(html.contains("&lt;script&gt;"));
}
