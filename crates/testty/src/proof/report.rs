//! Proof report collector and annotated text output.
//!
//! [`ProofReport`] accumulates [`ProofCapture`] entries during scenario
//! execution. Each capture records a terminal frame snapshot alongside its
//! label, description, dimensions, and optional assertion results.

use std::fmt::Write;
use std::path::Path;

use super::backend::{ProofBackend, RenderContext};
use crate::assertion::AssertionFailure;
use crate::diff::FrameDiff;
use crate::frame::TerminalFrame;

/// A single labeled capture collected during scenario execution.
///
/// Each capture preserves the full terminal frame data (text, colors,
/// and styles), dimensions, label, description, and optional assertion
/// results for proof rendering.
#[derive(Debug, Clone)]
pub struct ProofCapture {
    /// Short identifier for this capture step.
    pub label: String,
    /// Human-readable description of what this capture documents.
    pub description: String,
    /// Full terminal text at the moment of capture (plain text, no escapes).
    pub frame_text: String,
    /// ANSI-formatted bytes that reproduce the full frame state including
    /// colors and styles. Pass to [`TerminalFrame::new()`] with [`cols`]
    /// and [`rows`] to reconstruct a frame with full cell metadata.
    pub frame_bytes: Vec<u8>,
    /// Number of terminal columns at capture time.
    pub cols: u16,
    /// Number of terminal rows at capture time.
    pub rows: u16,
    /// Optional list of assertion results (pass/fail with description).
    pub assertions: Vec<AssertionResult>,
}

/// The outcome of a single assertion evaluated against a captured frame.
///
/// The `description` field is intentionally kept as a single-line summary
/// so the annotated text backend can render `[PASS]`/`[FAIL]` markers on
/// one row per assertion. When the assertion comes from the structured
/// matcher core (for example, through
/// [`ProofReport::record_soft_failure`]), the full
/// [`AssertionFailure`] is preserved on `failure` so HTML and other
/// structured backends can render `Expected`, `Region`, matched spans,
/// and the frame excerpt without reparsing the formatted message.
///
/// Kept as a regular struct (not `#[non_exhaustive]`) so downstream
/// crates that read [`ProofCapture::assertions`] can also push their own
/// entries with struct literals when they assemble custom proof data.
/// Because struct literals must name every field, every new field on
/// this type is a breaking change for downstream constructors and lands
/// in lockstep with a testty major version bump and a `CHANGELOG.md`
/// migration note.
#[derive(Debug, Clone)]
pub struct AssertionResult {
    /// Whether the assertion passed.
    pub passed: bool,
    /// Single-line human-readable description of the assertion.
    pub description: String,
    /// Structured failure context, when the result came from a `match_*`
    /// matcher. `None` for legacy single-line assertions added through
    /// [`ProofReport::add_assertion`].
    pub failure: Option<Box<AssertionFailure>>,
}

/// Errors that can occur during proof report generation.
///
/// Marked `#[non_exhaustive]` so future error variants stay non-breaking for
/// downstream callers that match on this type; such callers must include a
/// fallback `_` arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProofError {
    /// An I/O operation failed during proof output.
    #[error("proof I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A formatting error occurred during proof rendering.
    #[error("proof format error: {0}")]
    Format(String),
}

/// Collector for labeled captures produced during scenario execution.
///
/// Build a `ProofReport` by calling [`add_capture()`](ProofReport::add_capture)
/// for each labeled step, then render the report through a
/// [`ProofBackend`](super::backend::ProofBackend) or directly via
/// [`to_annotated_text()`](ProofReport::to_annotated_text).
#[derive(Debug, Clone)]
pub struct ProofReport {
    /// Human-readable name of the scenario that produced this report.
    pub scenario_name: String,
    /// Ordered list of captures collected during execution.
    pub captures: Vec<ProofCapture>,
    /// Diffs between consecutive captures, indexed by `(i, i+1)` pair.
    ///
    /// `diffs[i]` is the diff from `captures[i]` to `captures[i+1]`.
    /// The length is always `captures.len().saturating_sub(1)`.
    pub diffs: Vec<FrameDiff>,
}

impl ProofReport {
    /// Create an empty proof report for the given scenario name.
    pub fn new(scenario_name: impl Into<String>) -> Self {
        Self {
            scenario_name: scenario_name.into(),
            captures: Vec::new(),
            diffs: Vec::new(),
        }
    }

    /// Add a labeled capture from a terminal frame.
    ///
    /// If a previous capture exists, a [`FrameDiff`] between the previous
    /// and current frame is automatically computed and stored.
    pub fn add_capture(
        &mut self,
        label: impl Into<String>,
        description: impl Into<String>,
        frame: &TerminalFrame,
    ) {
        // Compute diff from previous capture's reconstructed frame.
        if let Some(previous) = self.captures.last() {
            let previous_frame =
                TerminalFrame::new(previous.cols, previous.rows, &previous.frame_bytes);
            self.diffs.push(FrameDiff::compute(&previous_frame, frame));
        }

        self.captures.push(ProofCapture {
            label: label.into(),
            description: description.into(),
            frame_text: frame.all_text(),
            frame_bytes: frame.contents_formatted(),
            cols: frame.cols(),
            rows: frame.rows(),
            assertions: Vec::new(),
        });
    }

    /// Attach an assertion result to the capture with the given label.
    ///
    /// Returns `true` if the label was found and the assertion was
    /// attached, `false` if no capture matches the label.
    pub fn add_assertion(
        &mut self,
        label: &str,
        passed: bool,
        description: impl Into<String>,
    ) -> bool {
        if let Some(capture) = self
            .captures
            .iter_mut()
            .find(|capture| capture.label == label)
        {
            capture.assertions.push(AssertionResult {
                passed,
                description: description.into(),
                failure: None,
            });

            return true;
        }

        false
    }

    /// Attach a soft-batched [`AssertionFailure`] to the most recent capture.
    ///
    /// Used by [`crate::assertion::SoftAssertions`] to route every
    /// recorded failure into [`ProofCapture::assertions`] on the most
    /// recently added capture, so a single capture can carry every batched
    /// failure for the proof report instead of just the first.
    ///
    /// The pushed [`AssertionResult`] uses the first line of the
    /// failure's pre-formatted `message` as `description` so the
    /// annotated text backend keeps a one-line `[FAIL] <summary>` shape,
    /// stores the full [`AssertionFailure`] on `failure` so structured
    /// backends (HTML, future renderers) can render `Expected`, `Region`,
    /// matched spans, and the frame excerpt without reparsing the
    /// formatted message, and is always marked failed.
    ///
    /// Returns `true` when at least one capture exists and the failure was
    /// attached, `false` when the report has no captures yet.
    pub fn record_soft_failure(&mut self, failure: &AssertionFailure) -> bool {
        if let Some(capture) = self.captures.last_mut() {
            let summary = failure
                .message
                .lines()
                .next()
                .unwrap_or(&failure.message)
                .to_string();
            capture.assertions.push(AssertionResult {
                passed: false,
                description: summary,
                failure: Some(Box::new(failure.clone())),
            });

            return true;
        }

        false
    }

    /// Render the report through a [`ProofBackend`] and write to `path`.
    ///
    /// This is the primary proof output method. Use
    /// [`to_annotated_text()`](Self::to_annotated_text) for in-memory text
    /// output.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError`] if rendering or I/O fails.
    pub fn save(&self, backend: &dyn ProofBackend, path: &Path) -> Result<(), ProofError> {
        let context = RenderContext::new(self, path);

        backend.render(&context)
    }

    /// Render the report as annotated plain text.
    ///
    /// Each capture is shown as a bordered frame dump with step number,
    /// label, description, and assertion markers.
    pub fn to_annotated_text(&self) -> String {
        let mut output = String::new();

        write_header(&mut output, &self.scenario_name);

        for (index, capture) in self.captures.iter().enumerate() {
            let step_number = index + 1;
            write_capture_section(&mut output, step_number, capture);
        }

        write_footer(&mut output, self.captures.len());

        output
    }
}

/// Write the report header with scenario name.
fn write_header(output: &mut String, scenario_name: &str) {
    let title = format!("Proof Report: {scenario_name}");
    let border = "=".repeat(title.len().max(60));

    let _ = writeln!(output, "{border}");
    let _ = writeln!(output, "{title}");
    let _ = writeln!(output, "{border}");
    let _ = writeln!(output);
}

/// Write one capture section with bordered frame dump and assertions.
fn write_capture_section(output: &mut String, step_number: usize, capture: &ProofCapture) {
    let heading = format!(
        "Step {step_number}: [{}] {}",
        capture.label, capture.description
    );
    let separator = "-".repeat(heading.len().max(60));

    let _ = writeln!(output, "{separator}");
    let _ = writeln!(output, "{heading}");
    let _ = writeln!(output, "  Terminal: {}x{}", capture.cols, capture.rows);
    let _ = writeln!(output, "{separator}");
    let _ = writeln!(output);

    // Frame text with left border.
    let frame_border = format!("+{}+", "-".repeat(usize::from(capture.cols) + 2));
    let _ = writeln!(output, "{frame_border}");
    for line in capture.frame_text.lines() {
        let padded = format!("{line:<width$}", width = usize::from(capture.cols));
        let _ = writeln!(output, "| {padded} |");
    }
    let _ = writeln!(output, "{frame_border}");
    let _ = writeln!(output);

    // Assertion results. The first line of `description` is rendered on
    // the `[PASS]`/`[FAIL]` row; any continuation lines (for example,
    // when a soft failure stored a single-line summary but a future
    // backend extends `description`) are indented under the marker so
    // they do not visually merge with the next assertion.
    if !capture.assertions.is_empty() {
        let _ = writeln!(output, "  Assertions:");
        for assertion in &capture.assertions {
            let marker = if assertion.passed { "PASS" } else { "FAIL" };
            let mut lines = assertion.description.lines();
            let first = lines.next().unwrap_or("");
            let _ = writeln!(output, "    [{marker}] {first}");
            for line in lines {
                let _ = writeln!(output, "           {line}");
            }
        }
        let _ = writeln!(output);
    }
}

/// Write the report footer with capture count.
fn write_footer(output: &mut String, capture_count: usize) {
    let _ = writeln!(output, "Total captures: {capture_count}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assertion::{Expected, SoftAssertions, match_not_visible, match_text_in_region};
    use crate::region::Region;

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

    #[test]
    fn frame_bytes_stores_formatted_output() {
        // Arrange — colored text.
        let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

        // Act
        let mut report = ProofReport::new("bytes_test");
        report.add_capture("colored", "Red text", &frame);

        // Assert — frame_bytes should contain ANSI escape sequences.
        let capture = &report.captures[0];
        assert!(!capture.frame_bytes.is_empty());
        assert!(capture.frame_bytes.len() > capture.frame_text.len());
    }
}
