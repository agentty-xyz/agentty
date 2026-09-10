use crate::assertion::{SoftAssertions, match_not_visible, match_text_in_region};
use crate::frame::TerminalFrame;
use crate::proof::report::ProofReport;
use crate::region::Region;

#[test]
fn soft_assertions_empty_does_not_panic_on_drop() {
    // Arrange / Act — drop without recording anything.
    {
        let _soft = SoftAssertions::default();
    }

    // Assert — reaching this point means drop did not panic.
}

#[test]
#[should_panic(expected = "SoftAssertions: 1 failure(s)")]
fn soft_assertions_single_failure_panics_on_drop_with_message() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let region = Region::new(20, 0, 60, 1);

    // Act / Assert — drop triggers the panic with the only recorded failure.
    let mut soft = SoftAssertions::new();
    soft.check(match_text_in_region(&frame, "Hello", &region));
}

#[test]
fn soft_assertions_format_aggregated_message_lists_each_failure_in_order() {
    // Arrange — record two failing checks against an empty region
    // plus one passing check, then drain the accumulator to suppress
    // the drop-time panic. Validating `format_aggregated_message`
    // directly avoids swapping the global panic hook (which would
    // suppress unrelated panics in other tests running in parallel)
    // while still pinning the exact rendered shape that `Drop` emits.
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let empty_region = Region::new(20, 0, 60, 1);
    let visible_region = Region::new(0, 0, 80, 1);

    let mut soft = SoftAssertions::new();
    soft.check(match_text_in_region(&frame, "Hello", &empty_region));
    soft.check(match_text_in_region(&frame, "Hello", &visible_region));
    soft.check(match_not_visible(&frame, "Hello"));
    let failures = soft.into_failures();

    // Act
    let message = SoftAssertions::format_aggregated_message(&failures);

    // Assert — header names the failure count, both failing checks
    // appear with their 1-based indices in record order, the passing
    // check is absent, and the failing-check ordering is preserved
    // across the whole string.
    assert_eq!(failures.len(), 2);
    assert!(
        message.starts_with("SoftAssertions: 2 failure(s) recorded:\n"),
        "unexpected header: {message}"
    );
    let not_found_index = message
        .find("[1/2]")
        .expect("first failure should be indexed [1/2]");
    let not_visible_index = message
        .find("[2/2]")
        .expect("second failure should be indexed [2/2]");
    assert!(not_found_index < not_visible_index);
    assert!(message[not_found_index..not_visible_index].contains("not found in region"));
    assert!(message[not_visible_index..].contains("NOT be visible"));
}

#[test]
fn soft_assertions_into_failures_suppresses_drop_panic() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let region = Region::new(20, 0, 60, 1);

    // Act — explicit consume should suppress the end-of-scope panic
    // even when failures were recorded.
    let mut soft = SoftAssertions::new();
    soft.check(match_text_in_region(&frame, "Hello", &region));
    let failures = soft.into_failures();

    // Assert — failures handed to the caller, no panic on drop.
    assert_eq!(failures.len(), 1);
}

#[test]
#[should_panic(expected = "requires at least one capture")]
fn soft_assertions_with_report_panics_when_no_capture_exists() {
    // Arrange — fresh report with no captures so binding is unsafe:
    // failures recorded later could never be routed into
    // `ProofCapture::assertions`.
    let mut report = ProofReport::new("empty-report");

    // Act — bind without first calling `add_capture`.
    let _soft = SoftAssertions::with_report(&mut report);

    // Assert — `with_report` panics before any check can run, so the
    // misconfiguration surfaces at the bind site instead of being
    // silently dropped from the proof report.
}

#[test]
fn soft_assertions_len_and_is_empty_track_recorded_failures() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let region = Region::new(20, 0, 60, 1);

    // Act / Assert
    let mut soft = SoftAssertions::new();
    assert!(soft.is_empty());
    assert_eq!(soft.len(), 0);

    soft.check(match_text_in_region(&frame, "Hello", &region));
    assert!(!soft.is_empty());
    assert_eq!(soft.len(), 1);

    // Drain to suppress the drop-time panic.
    let _ = soft.into_failures();
}

#[test]
fn assertion_failure_display_matches_legacy_panic_message() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let region = Region::new(20, 0, 60, 1);

    // Act
    let failure = match_text_in_region(&frame, "Hello", &region).expect_err("should be Err");

    // Assert — Display reproduces the panic-adapter message verbatim.
    assert_eq!(failure.to_string(), failure.message);
}
