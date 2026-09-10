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
    /// Optional list of assertion results (pass/fail with description).
    pub assertions: Vec<AssertionResult>,
    /// Number of terminal columns at capture time.
    pub cols: u16,
    /// Human-readable description of what this capture documents.
    pub description: String,
    /// ANSI-formatted bytes that reproduce the full frame state including
    /// colors and styles. Pass to [`TerminalFrame::new()`] with [`cols`]
    /// and [`rows`] to reconstruct a frame with full cell metadata.
    pub frame_bytes: Vec<u8>,
    /// Full terminal text at the moment of capture (plain text, no escapes).
    pub frame_text: String,
    /// Short identifier for this capture step.
    pub label: String,
    /// Number of terminal rows at capture time.
    pub rows: u16,
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
    /// Single-line human-readable description of the assertion.
    pub description: String,
    /// Structured failure context, when the result came from a `match_*`
    /// matcher. `None` for legacy single-line assertions added through
    /// [`ProofReport::add_assertion`].
    pub failure: Option<Box<AssertionFailure>>,
    /// Whether the assertion passed.
    pub passed: bool,
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
    /// Ordered list of captures collected during execution.
    pub captures: Vec<ProofCapture>,
    /// Diffs between consecutive captures, indexed by `(i, i+1)` pair.
    ///
    /// `diffs[i]` is the diff from `captures[i]` to `captures[i+1]`.
    /// The length is always `captures.len().saturating_sub(1)`.
    pub diffs: Vec<FrameDiff>,
    /// Human-readable name of the scenario that produced this report.
    pub scenario_name: String,
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
#[path = "report_test.rs"]
mod tests;
