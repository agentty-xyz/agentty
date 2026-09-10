use crate::assertion::{
    AssertionFailure, Expected, SoftAssertions, match_not_visible, match_text_in_region,
};
use crate::frame::TerminalFrame;
use crate::proof::report::ProofReport;
use crate::region::Region;

#[test]
fn record_soft_failure_attaches_to_latest_capture() {
    // Arrange — two captures so we can prove "latest" is targeted.
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let mut report = ProofReport::new("soft_failure_routing");
    report.add_capture("first", "First snapshot", &frame);
    report.add_capture("second", "Second snapshot", &frame);

    // Act
    let failure = AssertionFailure {
        message: "boom".to_string(),
        expected: Expected::TextInRegion {
            needle: "missing".to_string(),
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: String::new(),
    };
    let attached = report.record_soft_failure(&failure);

    // Assert — failure lands on the most recent capture, not the
    // first. The description is the single-line summary so the
    // annotated text marker shape stays clean, and the full
    // structured `AssertionFailure` is preserved on `failure` so
    // structured backends can render `Expected`, `Region`, matched
    // spans, and the frame excerpt without reparsing the message.
    assert!(attached);
    assert!(report.captures[0].assertions.is_empty());
    assert_eq!(report.captures[1].assertions.len(), 1);
    let result = &report.captures[1].assertions[0];
    assert!(!result.passed);
    assert_eq!(result.description, "boom");
    let stored = result
        .failure
        .as_deref()
        .expect("structured failure should be preserved");
    assert_eq!(stored.message, "boom");
    assert!(matches!(stored.expected, Expected::TextInRegion { .. }));
}

#[test]
fn record_soft_failure_keeps_description_single_line_for_multiline_message() {
    // Arrange — failure with a multi-line `message` mimicking what
    // `match_text_in_region` produces. Without the single-line
    // summary, the annotated text backend would lose the `[FAIL]`
    // indent on continuation lines and visually merge with the next
    // assertion.
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let mut report = ProofReport::new("soft_failure_multiline_summary");
    report.add_capture("only", "Only capture", &frame);
    let failure = AssertionFailure {
        message: "first line summary\n  detail line\n  another detail".to_string(),
        expected: Expected::TextInRegion {
            needle: "missing".to_string(),
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: String::new(),
    };

    // Act
    report.record_soft_failure(&failure);

    // Assert — `description` is the first line only, and the full
    // multi-line message is preserved on the structured `failure`.
    let result = &report.captures[0].assertions[0];
    assert_eq!(result.description, "first line summary");
    let stored = result.failure.as_deref().expect("failure preserved");
    assert!(stored.message.contains("detail line"));
    assert!(stored.message.contains("another detail"));
}

#[test]
fn record_soft_failure_returns_false_without_captures() {
    // Arrange
    let mut report = ProofReport::new("empty_report");
    let failure = AssertionFailure {
        message: "no captures yet".to_string(),
        expected: Expected::NotVisible {
            needle: "x".to_string(),
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: String::new(),
    };

    // Act
    let attached = report.record_soft_failure(&failure);

    // Assert
    assert!(!attached);
}

#[test]
fn soft_assertions_attach_each_failure_to_active_capture() {
    // Arrange — frame whose region excludes the needle so two soft
    // checks fail and one passes against the same capture.
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let visible_region = Region::new(0, 0, 80, 1);
    let empty_region = Region::new(20, 0, 60, 1);
    let mut report = ProofReport::new("soft_capture_routing");
    report.add_capture("only", "Only capture", &frame);

    // Act — bind soft accumulator to the report and run several checks.
    {
        let mut soft = SoftAssertions::with_report(&mut report);
        soft.check(match_text_in_region(&frame, "Hello", &empty_region));
        soft.check(match_text_in_region(&frame, "Hello", &visible_region));
        soft.check(match_not_visible(&frame, "Hello"));
        // Consume to suppress the end-of-scope panic; the routed
        // assertions on the proof report stay in place.
        let failures = soft.into_failures();
        assert_eq!(failures.len(), 2);
    }

    // Assert — both failures landed on the most recent capture in
    // record order, the passing check left no trace, descriptions
    // are single-line summaries (so the annotated text marker shape
    // stays clean), and the full structured `AssertionFailure` is
    // preserved on `failure` so structured backends can render
    // `Expected`, `Region`, matched spans, and the frame excerpt.
    let assertions = &report.captures[0].assertions;
    assert_eq!(assertions.len(), 2);
    assert!(assertions.iter().all(|result| !result.passed));
    assert!(assertions[0].description.contains("not found in region"));
    assert!(assertions[1].description.contains("NOT be visible"));
    assert!(!assertions[0].description.contains('\n'));
    assert!(!assertions[1].description.contains('\n'));
    assert!(matches!(
        assertions[0].failure.as_deref().map(|f| &f.expected),
        Some(Expected::TextInRegion { .. })
    ));
    assert!(matches!(
        assertions[1].failure.as_deref().map(|f| &f.expected),
        Some(Expected::NotVisible { .. })
    ));
}
