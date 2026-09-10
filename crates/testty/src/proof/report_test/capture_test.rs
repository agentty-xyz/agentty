use crate::frame::TerminalFrame;
use crate::proof::report::ProofReport;

#[test]
fn proof_report_collects_captures() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello, World!");

    // Act
    let mut report = ProofReport::new("test_scenario");
    report.add_capture("startup", "Application launched", &frame);
    report.add_capture("after_input", "User typed text", &frame);

    // Assert
    assert_eq!(report.captures.len(), 2);
    assert_eq!(report.captures[0].label, "startup");
    assert_eq!(report.captures[1].label, "after_input");
    assert!(report.captures[0].frame_text.contains("Hello, World!"));
}

#[test]
fn add_assertion_attaches_to_labeled_capture() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Test");
    let mut report = ProofReport::new("assert_test");
    report.add_capture("check", "Checking state", &frame);

    // Act
    report.add_assertion("check", true, "Text 'Test' is visible");
    report.add_assertion("check", false, "Color is blue");

    // Assert
    let capture = &report.captures[0];
    assert_eq!(capture.assertions.len(), 2);
    assert!(capture.assertions[0].passed);
    assert!(!capture.assertions[1].passed);
}

#[test]
fn add_assertion_targets_specific_capture() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Test");
    let mut report = ProofReport::new("targeted_test");
    report.add_capture("first", "First", &frame);
    report.add_capture("second", "Second", &frame);

    // Act
    let found = report.add_assertion("first", true, "targeted assertion");

    // Assert — assertion lands on "first", not "second".
    assert!(found);
    assert_eq!(report.captures[0].assertions.len(), 1);
    assert_eq!(
        report.captures[0].assertions[0].description,
        "targeted assertion"
    );
    assert!(report.captures[1].assertions.is_empty());
}

#[test]
fn add_assertion_returns_false_for_unknown_label() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Test");
    let mut report = ProofReport::new("miss_test");
    report.add_capture("existing", "Existing", &frame);

    // Act
    let found = report.add_assertion("nonexistent", true, "orphan");

    // Assert
    assert!(!found);
    assert!(report.captures[0].assertions.is_empty());
}

#[test]
fn proof_capture_stores_dimensions() {
    // Arrange
    let frame = TerminalFrame::new(120, 40, b"wide");

    // Act
    let mut report = ProofReport::new("dims");
    report.add_capture("wide_term", "Wide terminal", &frame);

    // Assert
    assert_eq!(report.captures[0].cols, 120);
    assert_eq!(report.captures[0].rows, 40);
}

#[test]
fn auto_diff_computed_between_consecutive_captures() {
    // Arrange
    let frame_a = TerminalFrame::new(80, 24, b"Hello");
    let frame_b = TerminalFrame::new(80, 24, b"World");

    // Act
    let mut report = ProofReport::new("diff_test");
    report.add_capture("before", "Before change", &frame_a);
    report.add_capture("after", "After change", &frame_b);

    // Assert — one diff between the two captures.
    assert_eq!(report.diffs.len(), 1);
    assert!(!report.diffs[0].is_identical());
}

#[test]
fn no_diff_for_single_capture() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Solo");

    // Act
    let mut report = ProofReport::new("single");
    report.add_capture("only", "Only capture", &frame);

    // Assert
    assert!(report.diffs.is_empty());
}

#[test]
fn identical_captures_produce_identical_diff() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Same");

    // Act
    let mut report = ProofReport::new("same");
    report.add_capture("first", "First", &frame);
    report.add_capture("second", "Second", &frame);

    // Assert
    assert_eq!(report.diffs.len(), 1);
    assert!(report.diffs[0].is_identical());
}

#[test]
fn frame_bytes_preserves_style_for_diff() {
    // Arrange — same text but different style (plain vs bold).
    let frame_a = TerminalFrame::new(80, 24, b"Hello");
    let frame_b = TerminalFrame::new(80, 24, b"\x1b[1mHello\x1b[0m");

    // Act
    let mut report = ProofReport::new("style_diff");
    report.add_capture("plain", "Plain text", &frame_a);
    report.add_capture("bold", "Bold text", &frame_b);

    // Assert — style change should be detected because frame_bytes
    // preserves ANSI formatting for accurate reconstruction.
    assert_eq!(report.diffs.len(), 1);
    assert!(!report.diffs[0].is_identical());
}

#[test]
fn frame_bytes_stores_formatted_output() {
    // Arrange — colored text.
    let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

    // Act
    let mut report = ProofReport::new("bytes_test");
    report.add_capture("colored", "Red text", &frame);

    // Assert — frame_bytes should contain ANSI escape sequences.
    let capture = &report.captures[0];
    assert_ne!(capture.frame_bytes, [] as [u8; 0]);
    assert!(capture.frame_bytes.len() > capture.frame_text.len());
}
