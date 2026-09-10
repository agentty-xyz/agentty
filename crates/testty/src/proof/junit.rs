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
    /// Full (possibly multi-line) message placed in the element body.
    detail: String,
    /// Single-line summary placed in the `message` attribute.
    summary: String,
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

                TestCaseOutcome::Failed(FailureEntry { detail, summary })
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
#[path = "junit_test.rs"]
mod tests;
