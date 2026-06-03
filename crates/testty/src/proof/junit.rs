//! JUnit-XML proof backend.
//!
//! [`JunitBackend`] renders a [`ProofReport`](super::report::ProofReport) as a
//! JUnit-XML document so non-Rust CI systems (which natively ingest JUnit-XML
//! test reports) can surface testty proof results as test cases and failures.
//! Each scenario becomes a `<testsuite>`, each assertion becomes a `<testcase>`
//! (passing, or with a `<failure>` child carrying the structured message), and
//! a capture with no assertions becomes a skipped `<testcase>` so a documented
//! step is not mistaken for a real passing check. Test-case names that would
//! otherwise collide gain a stable ` #N` suffix to keep each identity unique.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use super::backend::{ProofBackend, RenderContext};
use super::report::{ProofCapture, ProofError, ProofReport};

/// Renders a proof report as a JUnit-XML document.
///
/// The mapping is: one `<testsuites>` root wrapping a single `<testsuite>` for
/// the scenario, one `<testcase>` per assertion, and — for a capture with no
/// assertions — one skipped `<testcase>` named by the capture label. An
/// assertion whose
/// [`AssertionResult::passed`](super::report::AssertionResult::passed) is
/// `false` carries a `<failure>` child; an assertion-free capture carries a
/// `<skipped>` child. The `tests`, `failures`, and `skipped` counts on both the
/// `<testsuites>` and `<testsuite>` elements reflect the emitted test cases,
/// and colliding test-case names gain a stable ` #N` suffix so each
/// `<testcase>` keeps a unique identity for consumers that merge or drop
/// duplicates.
pub struct JunitBackend;

impl ProofBackend for JunitBackend {
    /// Write the JUnit-XML proof to the given output path.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError::Io`] if writing the file fails.
    fn render(&self, context: &RenderContext<'_>) -> Result<(), ProofError> {
        let xml = build_junit(context.report);
        std::fs::write(context.output, xml)?;

        Ok(())
    }
}

/// Build the complete JUnit-XML document from a proof report.
fn build_junit(report: &ProofReport) -> String {
    let test_cases = collect_test_cases(report);
    let total = test_cases.len();
    let failures = test_cases
        .iter()
        .filter(|entry| matches!(entry.outcome, TestCaseOutcome::Failed(_)))
        .count();
    let skipped = test_cases
        .iter()
        .filter(|entry| matches!(entry.outcome, TestCaseOutcome::Skipped))
        .count();

    let suite_name = escape_xml_attr(&report.scenario_name);

    let mut xml = String::with_capacity(256);
    let _ = writeln!(xml, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    let _ = writeln!(
        xml,
        "<testsuites name=\"{suite_name}\" tests=\"{total}\" failures=\"{failures}\" \
         skipped=\"{skipped}\">"
    );
    let _ = writeln!(
        xml,
        "  <testsuite name=\"{suite_name}\" tests=\"{total}\" failures=\"{failures}\" \
         skipped=\"{skipped}\">"
    );

    for entry in &test_cases {
        write_test_case(&mut xml, &suite_name, entry);
    }

    let _ = writeln!(xml, "  </testsuite>");
    let _ = writeln!(xml, "</testsuites>");

    xml
}

/// Write one `<testcase>` element: childless for a passing assertion, with a
/// `<failure>` child for a failed assertion, or with a `<skipped>` child for an
/// assertion-free capture step.
fn write_test_case(xml: &mut String, classname: &str, entry: &TestCaseEntry) {
    let name = escape_xml_attr(&entry.name);

    match &entry.outcome {
        TestCaseOutcome::Passed => {
            let _ = writeln!(
                xml,
                "    <testcase name=\"{name}\" classname=\"{classname}\"/>"
            );
        }
        TestCaseOutcome::Failed(failure) => {
            let _ = writeln!(
                xml,
                "    <testcase name=\"{name}\" classname=\"{classname}\">"
            );
            let _ = writeln!(
                xml,
                "      <failure message=\"{}\">{}</failure>",
                escape_xml_attr(&failure.summary),
                escape_xml_text(&failure.detail)
            );
            let _ = writeln!(xml, "    </testcase>");
        }
        TestCaseOutcome::Skipped => {
            let _ = writeln!(
                xml,
                "    <testcase name=\"{name}\" classname=\"{classname}\">"
            );
            let _ = writeln!(xml, "      <skipped/>");
            let _ = writeln!(xml, "    </testcase>");
        }
    }
}

/// A single JUnit-XML `<testcase>` entry derived from a capture or assertion.
struct TestCaseEntry {
    /// Display name shown to CI, combining the capture label and, when the
    /// case came from an assertion, the assertion description. Made unique
    /// across the report by [`disambiguate_names`].
    name: String,
    /// Rendered state of the test case.
    outcome: TestCaseOutcome,
}

/// The rendered outcome of a single `<testcase>`.
enum TestCaseOutcome {
    /// A passing assertion, emitted as a childless `<testcase>`.
    Passed,
    /// A failed assertion, emitted with a `<failure>` child.
    Failed(FailureEntry),
    /// An assertion-free capture step, emitted with a `<skipped>` child so CI
    /// does not count the documented step as a real passing check.
    Skipped,
}

/// The message pair rendered into a `<failure>` element.
struct FailureEntry {
    /// Single-line summary placed in the `message` attribute.
    summary: String,
    /// Full (possibly multi-line) message placed in the element body.
    detail: String,
}

/// Collect the ordered, name-disambiguated test-case entries for every capture.
fn collect_test_cases(report: &ProofReport) -> Vec<TestCaseEntry> {
    let mut test_cases: Vec<TestCaseEntry> = report
        .captures
        .iter()
        .flat_map(capture_test_cases)
        .collect();

    disambiguate_names(&mut test_cases);

    test_cases
}

/// Build the test-case entries contributed by a single capture.
///
/// A capture with assertions yields one entry per assertion (named
/// `"{label} / {description}"`, passing or failing); a capture without
/// assertions yields a single skipped entry named by its label so the step is
/// still documented without being counted as a real passing assertion.
fn capture_test_cases(capture: &ProofCapture) -> Vec<TestCaseEntry> {
    if capture.assertions.is_empty() {
        return vec![TestCaseEntry {
            name: capture.label.clone(),
            outcome: TestCaseOutcome::Skipped,
        }];
    }

    capture
        .assertions
        .iter()
        .map(|assertion| {
            let outcome = if assertion.passed {
                TestCaseOutcome::Passed
            } else {
                let detail = assertion.failure.as_deref().map_or_else(
                    || assertion.description.clone(),
                    |failure| failure.message.clone(),
                );
                let summary = detail.lines().next().unwrap_or(&detail).to_string();

                TestCaseOutcome::Failed(FailureEntry { summary, detail })
            };

            TestCaseEntry {
                name: format!("{} / {}", capture.label, assertion.description),
                outcome,
            }
        })
        .collect()
}

/// Append a stable ` #N` suffix to any test-case names that would otherwise
/// collide, so each `<testcase>` keeps a unique `(classname, name)` identity
/// for consumers that merge or drop duplicates. Names that appear only once
/// are left untouched.
fn disambiguate_names(test_cases: &mut [TestCaseEntry]) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for entry in test_cases.iter() {
        *counts.entry(entry.name.as_str()).or_insert(0) += 1;
    }

    let duplicates: HashSet<String> = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name.to_string())
        .collect();

    let mut seen: HashMap<String, usize> = HashMap::new();
    for entry in test_cases.iter_mut() {
        if duplicates.contains(&entry.name) {
            let index = seen.entry(entry.name.clone()).or_insert(0);
            *index += 1;
            entry.name = format!("{} #{index}", entry.name);
        }
    }
}

/// Escape `&`, `<`, and `>` for XML character data (element bodies).
fn escape_xml_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape `&`, `<`, `>`, `"`, and `'` for XML attribute values.
fn escape_xml_attr(text: &str) -> String {
    escape_xml_text(text)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assertion::{AssertionFailure, Expected};
    use crate::frame::TerminalFrame;

    /// Build a report with one capture whose single assertion carries a
    /// structured [`AssertionFailure`], mirroring the
    /// `SoftAssertions::with_report` routing flow.
    fn report_with_structured_failure(failure: &AssertionFailure) -> ProofReport {
        let frame = TerminalFrame::new(20, 3, b"Hello World");
        let mut report = ProofReport::new("structured_failure");
        report.add_capture("only", "Only capture", &frame);
        report.record_soft_failure(failure);

        report
    }

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
}
