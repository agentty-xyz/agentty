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
#[path = "html_test.rs"]
mod tests;
