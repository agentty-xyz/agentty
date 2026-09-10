use crate::frame::TerminalFrame;
use crate::proof::report::{AssertionResult, ProofReport};

#[test]
fn annotated_text_contains_scenario_name() {
    // Arrange
    let report = ProofReport::new("my_scenario");

    // Act
    let text = report.to_annotated_text();

    // Assert
    assert!(text.contains("Proof Report: my_scenario"));
}

#[test]
fn annotated_text_contains_step_labels_and_descriptions() {
    // Arrange
    let frame = TerminalFrame::new(40, 5, b"content");
    let mut report = ProofReport::new("labeled_test");
    report.add_capture("init", "Initial state", &frame);
    report.add_capture("done", "Final state", &frame);

    // Act
    let text = report.to_annotated_text();

    // Assert
    assert!(text.contains("Step 1: [init] Initial state"));
    assert!(text.contains("Step 2: [done] Final state"));
    assert!(text.contains("Total captures: 2"));
}

#[test]
fn annotated_text_contains_frame_content() {
    // Arrange
    let frame = TerminalFrame::new(40, 5, b"Hello proof");
    let mut report = ProofReport::new("frame_test");
    report.add_capture("snap", "Snapshot", &frame);

    // Act
    let text = report.to_annotated_text();

    // Assert
    assert!(text.contains("Hello proof"));
    assert!(text.contains("Terminal: 40x5"));
}

#[test]
fn annotated_text_contains_assertion_markers() {
    // Arrange
    let frame = TerminalFrame::new(40, 5, b"Test");
    let mut report = ProofReport::new("assert_output");
    report.add_capture("check", "Verify state", &frame);
    report.add_assertion("check", true, "text visible");
    report.add_assertion("check", false, "color match");

    // Act
    let text = report.to_annotated_text();

    // Assert
    assert!(text.contains("[PASS] text visible"));
    assert!(text.contains("[FAIL] color match"));
}

#[test]
fn annotated_text_indents_multiline_assertion_descriptions() {
    // Arrange — capture with a multi-line `description` on an
    // assertion (which can happen when a future caller fills in
    // pre-existing `add_assertion` plumbing with structured text).
    // The renderer must keep the `[FAIL]` marker on the first line
    // and indent continuation lines under the marker so they do not
    // visually merge with the next assertion.
    let frame = TerminalFrame::new(40, 5, b"Test");
    let mut report = ProofReport::new("multiline_render");
    report.add_capture("check", "Verify state", &frame);
    report
        .captures
        .last_mut()
        .expect("capture exists")
        .assertions
        .push(AssertionResult {
            passed: false,
            description: "first summary line\nsecond detail line".to_string(),
            failure: None,
        });
    report
        .captures
        .last_mut()
        .expect("capture exists")
        .assertions
        .push(AssertionResult {
            passed: true,
            description: "next assertion".to_string(),
            failure: None,
        });

    // Act
    let text = report.to_annotated_text();

    // Assert — the `[FAIL]` marker carries the first summary line,
    // the continuation line is indented under it, and the next
    // assertion's `[PASS]` marker is intact (not consumed by the
    // multi-line description above).
    assert!(text.contains("    [FAIL] first summary line"));
    assert!(text.contains("           second detail line"));
    assert!(text.contains("    [PASS] next assertion"));
}
