use super::{build_junit, escape_xml_attr, escape_xml_text};
use crate::assertion::{AssertionFailure, Expected};
use crate::frame::TerminalFrame;
use crate::proof::backend::{ProofBackend, RenderContext};
use crate::proof::junit::JunitBackend;
use crate::proof::report::ProofReport;
use crate::test_support::report_with_structured_failure;

#[test]
fn junit_starts_with_xml_declaration() {
    // Arrange
    let report = ProofReport::new("decl_scenario");

    // Act
    let xml = build_junit(&report);

    // Assert
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
}

#[test]
fn junit_wraps_scenario_in_testsuite() {
    // Arrange
    let report = ProofReport::new("my_scenario");

    // Act
    let xml = build_junit(&report);

    // Assert
    assert!(xml.contains("<testsuites name=\"my_scenario\""));
    assert!(xml.contains("<testsuite name=\"my_scenario\""));
    assert!(xml.contains("</testsuite>"));
    assert!(xml.contains("</testsuites>"));
}

#[test]
fn junit_passing_assertion_has_testcase_without_failure() {
    // Arrange
    let frame = TerminalFrame::new(20, 3, b"Test");
    let mut report = ProofReport::new("pass_scenario");
    report.add_capture("check", "Verify state", &frame);
    report.add_assertion("check", true, "text visible");

    // Act
    let xml = build_junit(&report);

    // Assert — a testcase exists for the assertion but no failure child.
    assert!(xml.contains("<testcase name=\"check / text visible\""));
    assert!(xml.contains("classname=\"pass_scenario\""));
    assert!(!xml.contains("<failure"));
}

#[test]
fn junit_failing_assertion_emits_failure_element() {
    // Arrange
    let frame = TerminalFrame::new(20, 3, b"Test");
    let mut report = ProofReport::new("fail_scenario");
    report.add_capture("check", "Verify state", &frame);
    report.add_assertion("check", false, "color match");

    // Act
    let xml = build_junit(&report);

    // Assert — the failed assertion produces a failure element whose
    // message attribute carries the assertion description.
    assert!(xml.contains("<testcase name=\"check / color match\""));
    assert!(xml.contains("<failure message=\"color match\""));
}

#[test]
fn junit_uses_structured_failure_message_in_body() {
    // Arrange — a structured failure with a multi-line message; the full
    // message must land in the failure element body, the first line in
    // the message attribute.
    let failure = AssertionFailure {
        message: "first line summary\n  detail line\n  another detail".to_string(),
        expected: Expected::TextInRegion {
            needle: "missing".to_string(),
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: String::new(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let xml = build_junit(&report);

    // Assert — attribute summary is the first line; body holds full text.
    assert!(xml.contains("<failure message=\"first line summary\""));
    assert!(xml.contains("detail line"));
    assert!(xml.contains("another detail"));
}

#[test]
fn junit_counts_total_tests_and_failures() {
    // Arrange — one passing and one failing assertion on the same capture.
    let frame = TerminalFrame::new(20, 3, b"Test");
    let mut report = ProofReport::new("count_scenario");
    report.add_capture("check", "Verify state", &frame);
    report.add_assertion("check", true, "passes");
    report.add_assertion("check", false, "fails");

    // Act
    let xml = build_junit(&report);

    // Assert — two test cases, one failure, on both suite levels.
    assert!(xml.contains("<testsuites name=\"count_scenario\" tests=\"2\" failures=\"1\""));
    assert!(xml.contains("<testsuite name=\"count_scenario\" tests=\"2\" failures=\"1\""));
}

#[test]
fn junit_capture_without_assertions_is_skipped() {
    // Arrange — a capture with no assertions documents a step but is not a
    // real passing check.
    let frame = TerminalFrame::new(20, 3, b"Test");
    let mut report = ProofReport::new("step_scenario");
    report.add_capture("launched", "App launched", &frame);

    // Act
    let xml = build_junit(&report);

    // Assert — the capture becomes a skipped testcase named by its label,
    // counted under `skipped` rather than as a passing assertion.
    assert!(xml.contains("<testcase name=\"launched\""));
    assert!(xml.contains("<skipped/>"));
    assert!(xml.contains("tests=\"1\" failures=\"0\" skipped=\"1\""));
    assert!(!xml.contains("<failure"));
}

#[test]
fn junit_disambiguates_duplicate_testcase_names() {
    // Arrange — two assertions sharing a description on the same capture
    // would otherwise produce identical testcase identities.
    let frame = TerminalFrame::new(20, 3, b"Test");
    let mut report = ProofReport::new("dup_scenario");
    report.add_capture("check", "Verify", &frame);
    report.add_assertion("check", true, "same name");
    report.add_assertion("check", true, "same name");

    // Act
    let xml = build_junit(&report);

    // Assert — each colliding name gains a stable index suffix.
    assert!(xml.contains("<testcase name=\"check / same name #1\""));
    assert!(xml.contains("<testcase name=\"check / same name #2\""));
}

#[test]
fn junit_keeps_unique_testcase_names_unsuffixed() {
    // Arrange — distinct assertion descriptions must stay suffix-free.
    let frame = TerminalFrame::new(20, 3, b"Test");
    let mut report = ProofReport::new("unique_scenario");
    report.add_capture("check", "Verify", &frame);
    report.add_assertion("check", true, "first");
    report.add_assertion("check", true, "second");

    // Act
    let xml = build_junit(&report);

    // Assert — no ` #N` disambiguation suffix is appended.
    assert!(xml.contains("<testcase name=\"check / first\""));
    assert!(xml.contains("<testcase name=\"check / second\""));
    assert!(!xml.contains(" #1\""));
}

#[test]
fn junit_escapes_xml_special_characters() {
    // Arrange — scenario name and assertion text with XML metacharacters.
    let frame = TerminalFrame::new(20, 3, b"Test");
    let mut report = ProofReport::new("a & b <tag>");
    report.add_capture("check", "Verify", &frame);
    report.add_assertion("check", false, "expected <x> & \"y\"");

    // Act
    let xml = build_junit(&report);

    // Assert — metacharacters are escaped, raw forms never leak.
    assert!(xml.contains("name=\"a &amp; b &lt;tag&gt;\""));
    assert!(xml.contains("&quot;y&quot;"));
    assert!(!xml.contains("<tag>"));
    assert!(!xml.contains("expected <x>"));
}

#[test]
fn junit_backend_writes_file() {
    // Arrange
    let frame = TerminalFrame::new(20, 3, b"File");
    let mut report = ProofReport::new("file_scenario");
    report.add_capture("snap", "Snapshot", &frame);
    report.add_assertion("snap", true, "content visible");

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let output_path = temp_dir.path().join("report.xml");

    // Act
    let backend = JunitBackend;
    backend
        .render(&RenderContext::new(&report, &output_path))
        .expect("render should succeed");

    // Assert
    assert!(output_path.exists());
    let content = std::fs::read_to_string(&output_path).expect("failed to read");
    assert!(content.contains("<testsuite name=\"file_scenario\""));
}

#[test]
fn escape_xml_text_escapes_core_metacharacters() {
    // Arrange / Act / Assert
    assert_eq!(escape_xml_text("a & b"), "a &amp; b");
    assert_eq!(escape_xml_text("<tag>"), "&lt;tag&gt;");
}

#[test]
fn escape_xml_attr_escapes_quotes_and_metacharacters() {
    // Arrange / Act / Assert
    assert_eq!(escape_xml_attr("\"q\""), "&quot;q&quot;");
    assert_eq!(escape_xml_attr("a'b"), "a&apos;b");
    assert_eq!(escape_xml_attr("x & <y>"), "x &amp; &lt;y&gt;");
}
