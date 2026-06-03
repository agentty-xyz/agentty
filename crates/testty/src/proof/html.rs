//! HTML report proof backend.
//!
//! [`HtmlBackend`] generates a self-contained HTML file with embedded
//! base64-encoded frame images, step-by-step narrative, diff summaries,
//! and assertion results. The output can be opened in any browser or
//! uploaded as a CI artifact.

use std::fmt::Write;
use std::io::Cursor;

use base64::Engine;
use image::ImageFormat;
use unicode_width::UnicodeWidthStr;

use super::backend::{ProofBackend, RenderContext};
use super::report::{AssertionResult, ProofCapture, ProofError, ProofReport};
use crate::assertion::{AssertionFailure, Expected};
use crate::frame::{CellColor, CellStyle, TerminalFrame};
use crate::locator::MatchedSpan;
use crate::renderer;

/// Renders a proof report as a self-contained HTML file.
///
/// Frame images are base64-encoded and inlined as `<img>` tags. Diff
/// summaries and assertion results are displayed alongside each step.
pub struct HtmlBackend;

impl ProofBackend for HtmlBackend {
    /// Render the proof report as self-contained HTML.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError`] if rendering or writing fails.
    fn render(&self, context: &RenderContext<'_>) -> Result<(), ProofError> {
        let html = build_html(context.report)?;
        std::fs::write(context.output, html)?;

        Ok(())
    }
}

/// Build the complete HTML document from a proof report.
fn build_html(report: &ProofReport) -> Result<String, ProofError> {
    let mut html = String::with_capacity(8192);

    write_html_header(&mut html, &report.scenario_name);

    for (index, capture) in report.captures.iter().enumerate() {
        let step_number = index + 1;
        let diff_summary = if index > 0 && index - 1 < report.diffs.len() {
            Some(report.diffs[index - 1].summary())
        } else {
            None
        };

        write_step_card(&mut html, step_number, capture, diff_summary.as_deref())?;
    }

    write_html_footer(&mut html, report.captures.len());

    Ok(html)
}

/// Write the HTML document header with inline CSS.
fn write_html_header(html: &mut String, scenario_name: &str) {
    let escaped_name = escape_html(scenario_name);

    let _ = write!(
        html,
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Proof Report: {escaped_name}</title>
<style>
body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace; background: #1a1a2e; color: #e0e0e0; margin: 0; padding: 20px; }}
h1 {{ color: #00d4ff; border-bottom: 2px solid #00d4ff; padding-bottom: 10px; }}
.step-card {{ background: #16213e; border: 1px solid #0f3460; border-radius: 8px; margin: 20px 0; padding: 20px; }}
.step-header {{ display: flex; align-items: center; gap: 12px; margin-bottom: 15px; }}
.step-number {{ background: #00d4ff; color: #1a1a2e; font-weight: bold; padding: 4px 12px; border-radius: 4px; font-size: 14px; }}
.step-label {{ color: #e94560; font-weight: bold; font-size: 16px; }}
.step-desc {{ color: #a0a0a0; font-size: 14px; }}
.frame-img {{ max-width: 100%; border: 1px solid #0f3460; border-radius: 4px; }}
.diff-section {{ background: #0d1b2a; padding: 12px; border-radius: 4px; margin-top: 12px; font-size: 13px; }}
.diff-section h4 {{ color: #ffd700; margin: 0 0 8px 0; }}
.diff-item {{ color: #b0b0b0; padding: 2px 0; }}
.assertions {{ margin-top: 12px; }}
.assertion {{ padding: 4px 0; font-size: 13px; }}
.pass {{ color: #00e676; }}
.pass::before {{ content: "\2713 "; }}
.fail {{ color: #ff5252; }}
.fail::before {{ content: "\2717 "; }}
.failure-detail {{ display: flex; flex-wrap: wrap; gap: 16px; margin: 6px 0 10px 18px; padding: 12px; background: #0d1b2a; border-left: 3px solid #ff5252; border-radius: 4px; font-size: 12px; color: #d0d0d0; }}
.failure-context {{ flex: 1 1 240px; min-width: 220px; }}
.failure-context-row {{ margin-bottom: 4px; }}
.failure-context-label {{ color: #ffd700; font-weight: bold; margin-right: 4px; }}
.failure-spans {{ margin-top: 6px; }}
.failure-spans-title {{ color: #ffd700; font-weight: bold; }}
.failure-spans ul {{ margin: 4px 0 0 0; padding-left: 18px; }}
.failure-spans li {{ color: #b0b0b0; padding: 1px 0; }}
.failure-frame {{ flex: 2 1 320px; min-width: 280px; }}
.failure-frame-title {{ color: #ffd700; font-weight: bold; margin-bottom: 4px; }}
.frame-excerpt {{ background: #0a0f1a; border: 1px solid #0f3460; border-radius: 4px; padding: 8px; margin: 0; font-family: ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace; font-size: 12px; line-height: 1.3; white-space: pre; overflow-x: auto; color: #c8c8c8; }}
.row-gutter {{ color: #606060; user-select: none; }}
.col-ruler {{ color: #606060; user-select: none; }}
.needle-hit {{ background: #5a1a1a; color: #ffd0d0; }}
.footer {{ text-align: center; color: #606060; margin-top: 30px; padding-top: 15px; border-top: 1px solid #0f3460; font-size: 12px; }}
.terminal-info {{ color: #606060; font-size: 12px; margin-bottom: 8px; }}
</style>
</head>
<body>
<h1>Proof Report: {escaped_name}</h1>
"#
    );
}

/// Write a single step card with frame image, diff, and assertions.
fn write_step_card(
    html: &mut String,
    step_number: usize,
    capture: &ProofCapture,
    diff_summary: Option<&[String]>,
) -> Result<(), ProofError> {
    let _ = writeln!(html, "<div class=\"step-card\">");
    let _ = writeln!(html, "<div class=\"step-header\">");
    let _ = writeln!(
        html,
        "<span class=\"step-number\">Step {step_number}</span>"
    );
    let _ = writeln!(
        html,
        "<span class=\"step-label\">[{}]</span>",
        escape_html(&capture.label)
    );
    let _ = writeln!(
        html,
        "<span class=\"step-desc\">{}</span>",
        escape_html(&capture.description)
    );
    let _ = writeln!(html, "</div>");

    // Terminal dimensions.
    let _ = writeln!(
        html,
        "<div class=\"terminal-info\">Terminal: {}x{}</div>",
        capture.cols, capture.rows
    );

    // Rendered frame image as base64.
    let base64_image = render_capture_to_base64(capture)?;
    let _ = writeln!(
        html,
        "<img class=\"frame-img\" src=\"data:image/png;base64,{base64_image}\" alt=\"Step \
         {step_number}: {}\" />",
        escape_html(&capture.label)
    );

    // Diff summary.
    if let Some(summaries) = diff_summary
        && !summaries.is_empty()
    {
        let _ = writeln!(html, "<div class=\"diff-section\">");
        let _ = writeln!(html, "<h4>Changes from previous step</h4>");
        for summary_line in summaries {
            let _ = writeln!(
                html,
                "<div class=\"diff-item\">{}</div>",
                escape_html(summary_line)
            );
        }
        let _ = writeln!(html, "</div>");
    }

    // Assertion results.
    if !capture.assertions.is_empty() {
        let _ = writeln!(html, "<div class=\"assertions\">");
        for assertion in &capture.assertions {
            write_assertion(html, assertion);
        }
        let _ = writeln!(html, "</div>");
    }

    let _ = writeln!(html, "</div>");

    Ok(())
}

/// Write one assertion result, including a structured detail block when
/// the entry carries a [`AssertionFailure`] from a `match_*` matcher.
///
/// Legacy entries pushed through [`ProofReport::add_assertion`] keep the
/// historical one-line `pass`/`fail` shape unchanged. Structured failures
/// add a side-by-side context-and-frame block underneath that surfaces
/// the [`Expected`] variant, the optional [`Region`], the matched spans,
/// and a row/col-gutter frame excerpt with needle hits highlighted.
fn write_assertion(html: &mut String, assertion: &AssertionResult) {
    let css_class = if assertion.passed { "pass" } else { "fail" };
    let _ = writeln!(
        html,
        "<div class=\"assertion {css_class}\">{}</div>",
        escape_html(&assertion.description)
    );

    if let Some(failure) = assertion.failure.as_deref() {
        write_failure_detail(html, failure);
    }
}

/// Write the side-by-side structured detail block for one
/// [`AssertionFailure`].
///
/// The left column shows the structured context ([`Expected`], optional
/// [`Region`], matched spans). The right column renders
/// [`AssertionFailure::frame_excerpt`] inside a `<pre>` with a two-row
/// column ruler and per-row gutters. When the [`Expected`] variant
/// carries a needle, every occurrence in the excerpt is wrapped in a
/// `needle-hit` span so the report shows where the matcher saw the text.
fn write_failure_detail(html: &mut String, failure: &AssertionFailure) {
    let _ = writeln!(html, "<div class=\"failure-detail\">");

    write_failure_context(html, failure);
    write_failure_frame(html, failure);

    let _ = writeln!(html, "</div>");
}

/// Write the structured context column with `Expected`, `Region`, and
/// matched-span entries.
fn write_failure_context(html: &mut String, failure: &AssertionFailure) {
    let _ = writeln!(html, "<div class=\"failure-context\">");

    let _ = writeln!(
        html,
        "<div class=\"failure-context-row\"><span \
         class=\"failure-context-label\">Expected:</span> {}</div>",
        escape_html(&format_expected(&failure.expected))
    );

    if let Some(region) = failure.region {
        let _ = writeln!(
            html,
            "<div class=\"failure-context-row\"><span \
             class=\"failure-context-label\">Region:</span> col={} row={} width={} height={}</div>",
            region.col, region.row, region.width, region.height
        );
    }

    if !failure.matched_spans.is_empty() {
        let _ = writeln!(html, "<div class=\"failure-spans\">");
        let _ = writeln!(
            html,
            "<div class=\"failure-spans-title\">Matched spans ({})</div>",
            failure.matched_spans.len()
        );
        let _ = writeln!(html, "<ul>");
        for span in &failure.matched_spans {
            let _ = writeln!(html, "<li>{}</li>", escape_html(&format_span(span)));
        }
        let _ = writeln!(html, "</ul>");
        let _ = writeln!(html, "</div>");
    }

    let _ = writeln!(html, "</div>");
}

/// Write the colored frame-excerpt column with row/col gutters.
///
/// When the failure is region-scoped, gutter labels are offset by the
/// region's `(col, row)` so reported coordinates match the live frame
/// instead of the local excerpt origin. The excerpt is also normalized
/// against `failure.region` when present: each line is padded to the
/// region's display width and missing trailing rows are added as blank
/// lines so an all-blank or partially blank region still renders with
/// row/col labels that span the full region instead of collapsing to an
/// empty `<pre>` (because [`crate::frame::TerminalFrame::text_in_region`]
/// trims trailing spaces and drops trailing empty lines).
fn write_failure_frame(html: &mut String, failure: &AssertionFailure) {
    let _ = writeln!(html, "<div class=\"failure-frame\">");
    let _ = writeln!(
        html,
        "<div class=\"failure-frame-title\">Frame excerpt</div>"
    );

    let lines = excerpt_lines(failure);

    if lines.is_empty() {
        let _ = writeln!(html, "<pre class=\"frame-excerpt\"></pre>");
        let _ = writeln!(html, "</div>");

        return;
    }

    let (start_col, start_row) = failure
        .region
        .map_or((0u16, 0u16), |region| (region.col, region.row));
    let needle = needle_from_expected(&failure.expected);
    let needle_for_line = (!needle.is_empty()).then_some(needle);
    let mut remaining_hits = highlight_limit(&failure.expected);

    let max_width = lines
        .iter()
        .map(|line| UnicodeWidthStr::width(line.as_str()))
        .max()
        .unwrap_or(0);

    let _ = writeln!(html, "<pre class=\"frame-excerpt\">");

    write_column_ruler(html, start_col, max_width);

    for (offset, line) in lines.iter().enumerate() {
        let row_label = u32::from(start_row) + u32::try_from(offset).unwrap_or(u32::MAX);
        let _ = write!(html, "<span class=\"row-gutter\">{row_label:>4} </span>");
        let consumed = write_line_with_highlights(html, line, needle_for_line, remaining_hits);
        if let Some(remaining) = remaining_hits.as_mut() {
            *remaining = remaining.saturating_sub(consumed);
        }
        let _ = writeln!(html);
    }

    let _ = writeln!(html, "</pre>");
    let _ = writeln!(html, "</div>");
}

/// Build the per-row excerpt the renderer paints, normalizing against
/// `failure.region` when it is present.
///
/// `text_in_region` trims trailing spaces and drops trailing empty
/// lines, which loses the geometry information the coordinate-aware
/// failure view needs. When the failure is region-scoped, this helper
/// pads every existing line to the region's display width and appends
/// blank lines until the row count matches `region.height`, so an
/// all-blank region still renders rulers and gutters that span the
/// region instead of collapsing to an empty `<pre>`. When the failure
/// has no region (for example `NotVisible` over the whole frame), the
/// excerpt is returned as-is so unrelated layouts are unaffected.
fn excerpt_lines(failure: &AssertionFailure) -> Vec<String> {
    let Some(region) = failure.region else {
        return failure.frame_excerpt.lines().map(str::to_string).collect();
    };

    let target_width = usize::from(region.width);
    let target_height = usize::from(region.height);

    let mut lines: Vec<String> = failure
        .frame_excerpt
        .lines()
        .map(|line| pad_to_display_width(line, target_width))
        .collect();

    while lines.len() < target_height {
        lines.push(" ".repeat(target_width));
    }

    lines
}

/// Pad `line` with trailing spaces until its terminal-cell display
/// width reaches `target`. Lines already at or beyond `target` are
/// returned unchanged so wide glyphs that overflow the region width are
/// preserved verbatim.
fn pad_to_display_width(line: &str, target: usize) -> String {
    let current = UnicodeWidthStr::width(line);
    if current >= target {
        return line.to_string();
    }

    let mut padded = String::with_capacity(line.len() + (target - current));
    padded.push_str(line);
    for _ in 0..(target - current) {
        padded.push(' ');
    }

    padded
}

/// Write the column ruler above the frame excerpt.
///
/// The bottom row prints the ones digit for every column. Above it, the
/// tens row marks the tens digit at every column whose absolute index
/// is a multiple of ten. When any displayed column reaches 100 or
/// higher, an additional hundreds row is emitted above the tens row,
/// rendering the hundreds digit at the same multiples-of-ten positions
/// so absolute coordinates beyond column 99 can be read by stacking the
/// digits at any tens-marked column. The hundreds row is suppressed
/// when the excerpt only spans columns under 100 to keep the existing
/// compact ruler for narrow terminals. All rows share the
/// four-character row-gutter prefix used by frame rows.
fn write_column_ruler(html: &mut String, start_col: u16, width: usize) {
    if width == 0 {
        return;
    }

    let last_offset = u32::try_from(width.saturating_sub(1)).unwrap_or(u32::MAX);
    let max_col = u32::from(start_col).saturating_add(last_offset);

    if max_col >= 100 {
        let _ = write!(html, "<span class=\"col-ruler\">     ");
        for offset in 0..width {
            let col = u32::from(start_col) + u32::try_from(offset).unwrap_or(u32::MAX);
            if col >= 100 && col % 10 == 0 {
                let _ = write!(html, "{}", (col / 100) % 10);
            } else {
                let _ = write!(html, " ");
            }
        }
        let _ = writeln!(html, "</span>");
    }

    let _ = write!(html, "<span class=\"col-ruler\">     ");
    for offset in 0..width {
        let col = u32::from(start_col) + u32::try_from(offset).unwrap_or(u32::MAX);
        if col % 10 == 0 {
            let _ = write!(html, "{}", (col / 10) % 10);
        } else {
            let _ = write!(html, " ");
        }
    }
    let _ = writeln!(html, "</span>");

    let _ = write!(html, "<span class=\"col-ruler\">     ");
    for offset in 0..width {
        let col = u32::from(start_col) + u32::try_from(offset).unwrap_or(u32::MAX);
        let _ = write!(html, "{}", col % 10);
    }
    let _ = writeln!(html, "</span>");
}

/// Write one frame-excerpt line, wrapping needle occurrences in a
/// `needle-hit` span when the `Expected` variant defines a needle.
///
/// `max_hits` caps how many raw needle occurrences may be highlighted on
/// this line: `None` means "no cap" (all occurrences are emphasized),
/// `Some(n)` keeps highlighting to the first `n` raw matches so
/// first-match-only matchers (`ForegroundColor`, `BackgroundColor`,
/// `Highlighted`, `NotHighlighted`) do not imply that secondary
/// occurrences in the excerpt were validated. Returns the number of raw
/// needle occurrences highlighted, so the caller can decrement a
/// cross-line budget.
///
/// Match discovery mirrors [`TerminalFrame::find_text`] by advancing one
/// character past each match start, so overlapping needles like `ana` in
/// `banana` are not silently skipped. Overlapping byte ranges are then
/// merged into a single highlight so the emitted HTML never contains
/// nested or overlapping `needle-hit` spans.
fn write_line_with_highlights(
    html: &mut String,
    line: &str,
    needle: Option<&str>,
    max_hits: Option<usize>,
) -> usize {
    let Some(needle) = needle.filter(|n| !n.is_empty()) else {
        let _ = write!(html, "{}", escape_html(line));

        return 0;
    };

    if max_hits == Some(0) {
        let _ = write!(html, "{}", escape_html(line));

        return 0;
    }

    let (ranges, raw_count) = needle_byte_ranges(line, needle, max_hits);
    if ranges.is_empty() {
        let _ = write!(html, "{}", escape_html(line));

        return raw_count;
    }

    let mut cursor = 0;
    for (start, end) in ranges {
        if cursor < start {
            let _ = write!(html, "{}", escape_html(&line[cursor..start]));
        }
        let _ = write!(
            html,
            "<span class=\"needle-hit\">{}</span>",
            escape_html(&line[start..end])
        );
        cursor = end;
    }
    if cursor < line.len() {
        let _ = write!(html, "{}", escape_html(&line[cursor..]));
    }

    raw_count
}

/// Collect the byte ranges of `needle` occurrences in `line`, advancing
/// one character past each match start so overlapping matches reported
/// by [`TerminalFrame::find_text`] are reflected in the HTML excerpt.
///
/// `max_hits` caps how many raw occurrences are collected before merge,
/// so first-match-only matchers can scope highlighting to the span the
/// assertion actually validated. Strictly overlapping ranges are merged
/// so the emitted HTML never contains nested `needle-hit` spans, while
/// adjacent matches (where one ends exactly where the next begins) stay
/// as separate spans so distinct hits remain distinguishable in the
/// report. Returns the merged byte ranges and the number of raw
/// occurrences that were collected, so the caller can decrement a
/// cross-line highlight budget.
fn needle_byte_ranges(
    line: &str,
    needle: &str,
    max_hits: Option<usize>,
) -> (Vec<(usize, usize)>, usize) {
    let mut raw = Vec::new();
    let mut search_start = 0;
    while let Some(offset) = line.get(search_start..).and_then(|tail| tail.find(needle)) {
        let match_start = search_start + offset;
        let match_end = match_start + needle.len();
        raw.push((match_start, match_end));
        if max_hits.is_some_and(|max| raw.len() >= max) {
            break;
        }

        let advance = line[match_start..].chars().next().map_or(1, char::len_utf8);
        search_start = match_start + advance;
    }

    let raw_count = raw.len();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(raw.len());
    for (start, end) in raw {
        if let Some(last) = merged.last_mut()
            && start < last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }

    (merged, raw_count)
}

/// Maximum number of raw needle occurrences to highlight in the frame
/// excerpt for a given [`Expected`] variant.
///
/// Returns `None` for matchers that validate every match so the renderer
/// emphasizes them all, and `Some(1)` for first-match-only matchers
/// (`ForegroundColor`, `BackgroundColor`, `Highlighted`,
/// `NotHighlighted`) so the report does not imply that secondary
/// occurrences in the excerpt were checked.
fn highlight_limit(expected: &Expected) -> Option<usize> {
    match expected {
        Expected::TextInRegion { .. }
        | Expected::NotVisible { .. }
        | Expected::MatchCount { .. } => None,
        Expected::ForegroundColor { .. }
        | Expected::BackgroundColor { .. }
        | Expected::Highlighted { .. }
        | Expected::NotHighlighted { .. } => Some(1),
    }
}

/// Produce a human-readable description of an [`Expected`] variant for
/// the structured failure detail header.
///
/// The returned string is a single line with no trailing punctuation so
/// the renderer can wrap it in a context row without normalization.
/// Adding a new variant to [`Expected`] is intentionally a compile-time
/// break here so the renderer always has a description for every shape.
fn format_expected(expected: &Expected) -> String {
    match expected {
        Expected::TextInRegion { needle } => {
            format!("text '{needle}' visible in region")
        }
        Expected::NotVisible { needle } => {
            format!("text '{needle}' not visible anywhere in frame")
        }
        Expected::MatchCount { needle, count } => {
            format!("text '{needle}' to appear exactly {count} time(s)")
        }
        Expected::ForegroundColor { needle, color } => {
            format!(
                "first match of '{needle}' with foreground color {}",
                format_color(*color)
            )
        }
        Expected::BackgroundColor { needle, color } => {
            format!(
                "first match of '{needle}' with background color {}",
                format_color(*color)
            )
        }
        Expected::Highlighted { needle } => {
            format!("first match of '{needle}' to be highlighted")
        }
        Expected::NotHighlighted { needle } => {
            format!("first match of '{needle}' to not be highlighted")
        }
    }
}

/// Extract the search needle from an [`Expected`] variant for highlight
/// rendering. Every variant has a textual target today; new variants
/// without one should return `""` here so the renderer skips
/// highlighting.
fn needle_from_expected(expected: &Expected) -> &str {
    match expected {
        Expected::TextInRegion { needle }
        | Expected::NotVisible { needle }
        | Expected::MatchCount { needle, .. }
        | Expected::ForegroundColor { needle, .. }
        | Expected::BackgroundColor { needle, .. }
        | Expected::Highlighted { needle }
        | Expected::NotHighlighted { needle } => needle.as_str(),
    }
}

/// Format a [`MatchedSpan`] entry for the structured failure detail.
///
/// Includes the actual `foreground`, `background`, and `style` flags on
/// the span when they differ from defaults so color- and highlight-style
/// failures show the actual cell state alongside the [`Expected`]
/// description, instead of forcing readers back to the assertion-line
/// summary for the actual values.
fn format_span(span: &MatchedSpan) -> String {
    let mut out = format!(
        "'{}' at col={} row={} width={}",
        span.text, span.rect.col, span.rect.row, span.rect.width
    );

    if let Some(fg) = span.foreground {
        let _ = write!(out, " fg={}", format_color(fg));
    }
    if let Some(bg) = span.background {
        let _ = write!(out, " bg={}", format_color(bg));
    }

    let style_label = format_style(span.style);
    if !style_label.is_empty() {
        let _ = write!(out, " style={style_label}");
    }

    out
}

/// Render the non-default attribute flags from a [`CellStyle`] as a
/// short comma-separated label, or an empty string when no flags are
/// set. Used by [`format_span`] so highlight-style failures expose the
/// actual style bits without dumping a `Debug` blob into the report.
fn format_style(style: CellStyle) -> String {
    let mut flags: Vec<&'static str> = Vec::new();
    if style.bold() {
        flags.push("bold");
    }
    if style.italic() {
        flags.push("italic");
    }
    if style.underline() {
        flags.push("underline");
    }
    if style.inverse() {
        flags.push("inverse");
    }
    if style.dim() {
        flags.push("dim");
    }

    flags.join(",")
}

/// Format a [`CellColor`] as a readable `rgb(r, g, b)` triple.
fn format_color(color: CellColor) -> String {
    format!("rgb({}, {}, {})", color.red, color.green, color.blue)
}

/// Write the HTML document footer.
fn write_html_footer(html: &mut String, capture_count: usize) {
    let _ = write!(
        html,
        r#"<div class="footer">Total captures: {capture_count} | Generated by testty</div>
</body>
</html>
"#
    );
}

/// Render a capture's frame to a base64-encoded PNG string.
fn render_capture_to_base64(capture: &ProofCapture) -> Result<String, ProofError> {
    let frame = TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes);
    let image = renderer::render_to_image(&frame);

    let mut png_bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut png_bytes, ImageFormat::Png)
        .map_err(|err| ProofError::Format(err.to_string()))?;

    let encoded = base64::engine::general_purpose::STANDARD.encode(png_bytes.into_inner());

    Ok(encoded)
}

/// Escape special HTML characters.
pub(super) fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::CellStyle;
    use crate::region::Region;

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

    /// Build a report with one capture and one structured failure routed
    /// through `record_soft_failure`, mirroring the
    /// `SoftAssertions::with_report` flow.
    fn report_with_structured_failure(failure: &AssertionFailure) -> ProofReport {
        let frame = TerminalFrame::new(20, 3, b"Hello World");
        let mut report = ProofReport::new("structured_failure");
        report.add_capture("only", "Only capture", &frame);
        report.record_soft_failure(failure);

        report
    }

    #[test]
    fn html_renders_structured_failure_expected_and_region() {
        // Arrange — failure whose region was scoped to row 0 columns 0..20.
        let region = Region::new(0, 0, 20, 1);
        let failure = AssertionFailure {
            message: "text 'Goodbye' not found in region (col=0, row=0, 20x1)".to_string(),
            expected: Expected::TextInRegion {
                needle: "Goodbye".to_string(),
            },
            region: Some(region),
            matched_spans: Vec::new(),
            frame_excerpt: "Hello World         ".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — context column shows the Expected description and the
        // structured Region coordinates, and the failure-detail wrapper
        // exists.
        assert!(html.contains("class=\"failure-detail\""));
        assert!(html.contains("Expected:"));
        assert!(html.contains("text 'Goodbye' visible in region"));
        assert!(html.contains("Region:"));
        assert!(html.contains("col=0 row=0 width=20 height=1"));
    }

    #[test]
    fn html_renders_matched_spans_when_present() {
        // Arrange — `match_not_visible` style failure with two recorded
        // matches the renderer should list under the spans heading.
        let span_a = MatchedSpan {
            text: "Hello".to_string(),
            rect: Region::new(0, 0, 5, 1),
            foreground: None,
            background: None,
            style: CellStyle::default(),
        };
        let span_b = MatchedSpan {
            text: "Hello".to_string(),
            rect: Region::new(6, 1, 5, 1),
            foreground: None,
            background: None,
            style: CellStyle::default(),
        };
        let failure = AssertionFailure {
            message: "Expected text 'Hello' to NOT be visible".to_string(),
            expected: Expected::NotVisible {
                needle: "Hello".to_string(),
            },
            region: None,
            matched_spans: vec![span_a, span_b],
            frame_excerpt: "Hello World".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — both span entries render under the spans list, and
        // the section title carries the count.
        assert!(html.contains("Matched spans (2)"));
        assert!(html.contains("'Hello' at col=0 row=0 width=5"));
        assert!(html.contains("'Hello' at col=6 row=1 width=5"));
    }

    #[test]
    fn html_frame_excerpt_uses_region_offsets_for_row_gutter() {
        // Arrange — region scoped to row 5 starting at column 10 so the
        // gutter labels must reflect those offsets, not the local
        // excerpt origin.
        let region = Region::new(10, 5, 11, 2);
        let failure = AssertionFailure {
            message: "missing".to_string(),
            expected: Expected::TextInRegion {
                needle: "Goodbye".to_string(),
            },
            region: Some(region),
            matched_spans: Vec::new(),
            frame_excerpt: "first line \nsecond line".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — first frame row uses the region's row index (5), the
        // second uses 6, and the column ruler is anchored to col 10.
        assert!(html.contains("class=\"row-gutter\">   5 "));
        assert!(html.contains("class=\"row-gutter\">   6 "));
        assert!(html.contains("class=\"col-ruler\""));
    }

    #[test]
    fn html_highlights_needle_occurrences_in_frame_excerpt() {
        // Arrange — `not visible` failure whose excerpt contains the
        // needle twice; both occurrences should be wrapped in
        // `needle-hit` spans so the report shows where the text was
        // found.
        let failure = AssertionFailure {
            message: "found two".to_string(),
            expected: Expected::NotVisible {
                needle: "Hello".to_string(),
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: "Hello world Hello again".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert
        assert_eq!(html.matches("class=\"needle-hit\">Hello").count(), 2);
    }

    #[test]
    fn html_legacy_assertions_have_no_failure_detail() {
        // Arrange — legacy entry pushed without a structured failure
        // should not produce the structured detail block.
        let frame = TerminalFrame::new(20, 3, b"Test");
        let mut report = ProofReport::new("legacy_no_detail");
        report.add_capture("check", "Check", &frame);
        report.add_assertion("check", false, "color match");

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — legacy `add_assertion` carries no structured failure
        // so the failure-detail wrapper must not appear.
        assert!(html.contains("class=\"assertion fail\""));
        assert!(!html.contains("class=\"failure-detail\""));
    }

    #[test]
    fn html_renders_match_count_expectation() {
        // Arrange — `MatchCount` describes a count expectation that the
        // renderer should surface in the Expected line.
        let failure = AssertionFailure {
            message: "wrong count".to_string(),
            expected: Expected::MatchCount {
                needle: "TODO".to_string(),
                count: 3,
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: String::new(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert
        assert!(html.contains("text 'TODO' to appear exactly 3 time(s)"));
    }

    #[test]
    fn html_renders_color_expectation_with_rgb() {
        // Arrange — color expectations should render the RGB triple so
        // the failure column links the structured `CellColor` to the
        // human-readable description.
        let failure = AssertionFailure {
            message: "wrong color".to_string(),
            expected: Expected::ForegroundColor {
                needle: "Save".to_string(),
                color: CellColor::new(255, 0, 0),
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: String::new(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — wording attributes the expectation to the first
        // match so readers do not assume every occurrence of the needle
        // was checked.
        assert!(html.contains("first match of 'Save' with foreground color rgb(255, 0, 0)"));
    }

    #[test]
    fn html_format_span_includes_actual_fg_bg_and_style() {
        // Arrange — color failure whose first matched span carries the
        // actual foreground/background and `bold` style. The detail
        // block must surface those actual attributes alongside the
        // structured expectation so readers can compare expected vs.
        // actual without re-reading the assertion-line summary.
        let foreground = CellColor::new(10, 20, 30);
        let background = CellColor::new(40, 50, 60);
        let span = MatchedSpan {
            text: "Save".to_string(),
            rect: Region::new(2, 1, 4, 1),
            foreground: Some(foreground),
            background: Some(background),
            style: CellStyle::from_raw(0b0000_0001),
        };
        let failure = AssertionFailure {
            message: "Text 'Save' at (2, 1) has foreground Some(...), expected ...".to_string(),
            expected: Expected::ForegroundColor {
                needle: "Save".to_string(),
                color: CellColor::new(255, 0, 0),
            },
            region: None,
            matched_spans: vec![span],
            frame_excerpt: "Save".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert
        assert!(
            html.contains(
                "'Save' at col=2 row=1 width=4 fg=rgb(10, 20, 30) bg=rgb(40, 50, 60) style=bold"
            ),
            "expected matched-span to include actual fg/bg/style in:\n{html}"
        );
    }

    #[test]
    fn html_format_span_omits_unset_color_and_style_fields() {
        // Arrange — span without color or style flags should render the
        // base coordinates only, with no trailing fg/bg/style fragment.
        let span = MatchedSpan {
            text: "Save".to_string(),
            rect: Region::new(0, 0, 4, 1),
            foreground: None,
            background: None,
            style: CellStyle::default(),
        };
        let failure = AssertionFailure {
            message: "missing".to_string(),
            expected: Expected::NotVisible {
                needle: "Save".to_string(),
            },
            region: None,
            matched_spans: vec![span],
            frame_excerpt: "Save".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — list entry stops after `width=` and never contains an
        // empty `fg=`, `bg=`, or `style=` fragment.
        assert!(html.contains("'Save' at col=0 row=0 width=4</li>"));
        assert!(!html.contains("fg="));
        assert!(!html.contains("bg="));
        assert!(!html.contains("style="));
    }

    #[test]
    fn html_highlights_overlapping_needle_matches() {
        // Arrange — `TerminalFrame::find_text` reports `ana` twice in
        // `banana` because it advances one character past each match.
        // The HTML renderer must mirror that overlap semantics so the
        // excerpt does not silently drop the second match.
        let failure = AssertionFailure {
            message: "found two".to_string(),
            expected: Expected::NotVisible {
                needle: "ana".to_string(),
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: "banana".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — the two overlapping matches collapse into one span
        // covering the union of touched bytes, but every cell that any
        // match touched is highlighted (no second `ana` is dropped).
        assert!(
            html.contains("<span class=\"needle-hit\">anana</span>"),
            "expected merged overlapping highlight in:\n{html}"
        );
    }

    #[test]
    fn html_column_ruler_uses_terminal_cell_width_for_wide_glyphs() {
        // Arrange — the excerpt contains a wide CJK glyph that occupies
        // two terminal cells, and the surrounding region is 5 cells
        // wide. The ruler must size itself by display width so labeled
        // columns line up with the underlying terminal cells: padding
        // the line to 5 display cells with `chars().count()` would
        // overshoot by one cell (2 chars + 3 spaces = 5 chars but 6
        // display cells), and sizing the ruler from `chars().count()`
        // would undershoot to two labels.
        let failure = AssertionFailure {
            message: "missing".to_string(),
            expected: Expected::TextInRegion {
                needle: "Goodbye".to_string(),
            },
            region: Some(Region::new(0, 0, 5, 1)),
            matched_spans: Vec::new(),
            frame_excerpt: "中a".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — the ones-row ruler labels exactly 5 cells (`01234`)
        // because the line is padded to the region's 5 display columns
        // and the ruler is sized from the resulting display width, not
        // from the raw character count.
        assert!(
            html.contains("<span class=\"col-ruler\">     01234</span>"),
            "expected width-5 ones-row ruler in:\n{html}"
        );
    }

    #[test]
    fn html_column_ruler_renders_hundreds_row_past_column_99() {
        // Arrange — region anchored at column 100 with width 21 so the
        // ruler must label cells 100..=120. Without a hundreds row,
        // column 120 would render as `2` (tens) over `0` (ones) and be
        // indistinguishable from absolute column 20.
        let failure = AssertionFailure {
            message: "missing".to_string(),
            expected: Expected::TextInRegion {
                needle: "Goodbye".to_string(),
            },
            region: Some(Region::new(100, 0, 21, 1)),
            matched_spans: Vec::new(),
            frame_excerpt: "x".repeat(21),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — three ruler rows are emitted. The hundreds row marks
        // columns 100, 110, 120 with `1`; the tens row marks 100→`0`,
        // 110→`1`, 120→`2`; the ones row repeats `0..=9` and ends with
        // `0` at column 120. Stacking the three rows at column 120
        // recovers `120` instead of the ambiguous `20`.
        assert!(
            html.contains("<span class=\"col-ruler\">     1         1         1</span>"),
            "expected hundreds row marking cols 100/110/120 in:\n{html}"
        );
        assert!(
            html.contains("<span class=\"col-ruler\">     0         1         2</span>"),
            "expected tens row marking cols 100/110/120 in:\n{html}"
        );
        assert!(
            html.contains("<span class=\"col-ruler\">     012345678901234567890</span>"),
            "expected ones row spanning 21 cells in:\n{html}"
        );
    }

    #[test]
    fn html_column_ruler_omits_hundreds_row_under_column_100() {
        // Arrange — narrow ruler entirely under column 100 must keep the
        // existing two-row layout so the hundreds row stays opt-in for
        // excerpts that actually need it.
        let failure = AssertionFailure {
            message: "missing".to_string(),
            expected: Expected::TextInRegion {
                needle: "Goodbye".to_string(),
            },
            region: Some(Region::new(0, 0, 3, 1)),
            matched_spans: Vec::new(),
            frame_excerpt: "abc".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — exactly the two original ruler rows are emitted.
        assert_eq!(html.matches("class=\"col-ruler\"").count(), 2);
    }

    #[test]
    fn html_renders_adjacent_needle_matches_as_separate_spans() {
        // Arrange — adjacent matches share an edge but do not overlap.
        // For `needle = "ab"` in `abab`, the matcher reports two
        // distinct hits at byte ranges 0..2 and 2..4. The renderer must
        // keep them as two separate `needle-hit` spans so the report
        // shows the matcher saw two occurrences instead of one long
        // run.
        let failure = AssertionFailure {
            message: "found two".to_string(),
            expected: Expected::NotVisible {
                needle: "ab".to_string(),
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: "abab".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — two distinct spans, not one merged `abab` span.
        assert_eq!(
            html.matches("<span class=\"needle-hit\">ab</span>").count(),
            2,
            "expected two adjacent highlights in:\n{html}"
        );
        assert!(
            !html.contains("<span class=\"needle-hit\">abab</span>"),
            "adjacent matches must not collapse into one span in:\n{html}"
        );
    }

    #[test]
    fn html_first_match_only_expectation_highlights_only_first_occurrence() {
        // Arrange — `ForegroundColor` validates only the first match of
        // `needle`. The excerpt may show the needle multiple times, but
        // highlighting every occurrence would falsely imply the matcher
        // checked them all. The renderer must emphasize only the first
        // hit across the whole excerpt.
        let failure = AssertionFailure {
            message: "wrong fg".to_string(),
            expected: Expected::ForegroundColor {
                needle: "Save".to_string(),
                color: CellColor::new(255, 0, 0),
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: "Save first\nSave second".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — exactly one needle highlight even though the excerpt
        // contains two textual occurrences of `Save`.
        assert_eq!(
            html.matches("<span class=\"needle-hit\">Save</span>")
                .count(),
            1,
            "expected only the first occurrence to be highlighted in:\n{html}"
        );
    }

    #[test]
    fn html_highlight_limit_classifies_expected_variants() {
        // Arrange / Act / Assert — first-match-only matchers cap at 1,
        // every-match matchers stay unbounded so the renderer cannot
        // silently suppress hits when new variants are added.
        assert_eq!(
            highlight_limit(&Expected::TextInRegion {
                needle: "x".to_string(),
            }),
            None
        );
        assert_eq!(
            highlight_limit(&Expected::NotVisible {
                needle: "x".to_string(),
            }),
            None
        );
        assert_eq!(
            highlight_limit(&Expected::MatchCount {
                needle: "x".to_string(),
                count: 2,
            }),
            None
        );
        assert_eq!(
            highlight_limit(&Expected::ForegroundColor {
                needle: "x".to_string(),
                color: CellColor::new(0, 0, 0),
            }),
            Some(1)
        );
        assert_eq!(
            highlight_limit(&Expected::BackgroundColor {
                needle: "x".to_string(),
                color: CellColor::new(0, 0, 0),
            }),
            Some(1)
        );
        assert_eq!(
            highlight_limit(&Expected::Highlighted {
                needle: "x".to_string(),
            }),
            Some(1)
        );
        assert_eq!(
            highlight_limit(&Expected::NotHighlighted {
                needle: "x".to_string(),
            }),
            Some(1)
        );
    }

    #[test]
    fn html_renders_all_blank_region_with_full_dimensions() {
        // Arrange — region scoped to a 2-row × 8-col area where every
        // cell is blank. `text_in_region` collapses that to an empty
        // string by trimming trailing spaces and dropping trailing
        // empty lines, but the renderer must still surface the
        // region's geometry so location-sensitive debugging keeps
        // working when the failure is "the region is empty".
        let region = Region::new(5, 3, 8, 2);
        let failure = AssertionFailure {
            message: "missing".to_string(),
            expected: Expected::TextInRegion {
                needle: "Goodbye".to_string(),
            },
            region: Some(region),
            matched_spans: Vec::new(),
            frame_excerpt: String::new(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — both region rows render with their absolute row
        // labels (3 and 4), the column ruler is anchored to the
        // region's start column (5), and the `<pre>` is not the empty
        // shell that the previous renderer emitted for blank excerpts.
        assert!(html.contains("class=\"row-gutter\">   3 "));
        assert!(html.contains("class=\"row-gutter\">   4 "));
        assert!(html.contains("class=\"col-ruler\""));
        assert!(!html.contains("<pre class=\"frame-excerpt\"></pre>"));
    }

    #[test]
    fn html_pads_partially_blank_region_to_full_width() {
        // Arrange — region width is 12 cells but the first row only
        // contains `hi` before `text_in_region` trims trailing spaces.
        // The ruler must still span the full region width so column
        // labels reach the right edge of the region.
        let region = Region::new(0, 0, 12, 1);
        let failure = AssertionFailure {
            message: "missing".to_string(),
            expected: Expected::TextInRegion {
                needle: "Goodbye".to_string(),
            },
            region: Some(region),
            matched_spans: Vec::new(),
            frame_excerpt: "hi".to_string(),
        };
        let report = report_with_structured_failure(&failure);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — the ones-row ruler labels all 12 columns (`0..=9`,
        // then `0`, `1`), proving the renderer padded the line out to
        // the region's display width before sizing the ruler.
        assert!(
            html.contains("<span class=\"col-ruler\">     012345678901</span>"),
            "expected width-12 ones-row ruler in:\n{html}"
        );
    }
}
