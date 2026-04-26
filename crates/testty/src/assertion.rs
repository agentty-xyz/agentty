//! Matcher APIs for terminal frame assertions.
//!
//! Provides two layers over a [`TerminalFrame`]:
//!
//! - **`match_*` matchers** return `Result<(), Box<AssertionFailure>>` with
//!   structured failure context, so callers can compose, retry, or batch
//!   assertions without `catch_unwind`. The failure is boxed so the `Ok` path
//!   stays cheap and the `Result` enum stays small.
//! - **`assert_*` matchers** are thin panic adapters that delegate to the
//!   matching `match_*` and panic with the same byte-compatible failure message
//!   the previous direct-`assert!` implementations produced. They exist so
//!   historical tests do not have to change shape. [`assert_match_count`] is
//!   the one carve-out: it keeps using `assert_eq!` directly so the macro's
//!   standard `left`/`right` block stays in the panic output for downstream
//!   snapshots that match on the legacy framing.
//!
//! Failure context (matched rects, expected regions, actual regions, and
//! relevant colors) lives on [`AssertionFailure`], which is part of the
//! public API and re-exported from [`crate::prelude`].

use std::fmt;
use std::fmt::Write;

use crate::frame::{CellColor, TerminalFrame};
use crate::locator::MatchedSpan;
use crate::region::Region;

/// Result alias for [`AssertionFailure`]-returning matchers.
///
/// Returned by every `match_*` function in this module so callers can
/// compose, retry, or accumulate failures without unwinding. The failure
/// is boxed so the `Ok` path stays a single pointer-sized return.
pub type MatchResult = Result<(), Box<AssertionFailure>>;

/// Structured failure produced by the `match_*` matchers.
///
/// `AssertionFailure` carries the same context the previous panic strings
/// carried so callers can render the failure themselves, batch multiple
/// failures, or surface them into a [`crate::proof::report::ProofReport`]
/// without losing information.
///
/// The `Display` implementation reproduces the legacy panic message
/// byte-for-byte, so the [`assert_text_in_region`]-style panic adapters
/// stay observably identical to the pre-refactor surface.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AssertionFailure {
    /// Pre-formatted human-readable message, identical to the legacy
    /// panic string. Stored verbatim so adapters can `panic!("{e}")`.
    pub message: String,
    /// What was being asserted, in structured form.
    pub expected: Expected,
    /// Region the assertion was scoped to, when applicable.
    pub region: Option<Region>,
    /// Spans matched at evaluation time. Empty for "not found" failures.
    pub matched_spans: Vec<MatchedSpan>,
    /// Pre-formatted excerpt of the relevant frame area, so renderers can
    /// show what was on screen without re-walking the frame.
    pub frame_excerpt: String,
}

/// Structured description of what a `match_*` matcher was checking.
///
/// Used by [`AssertionFailure::expected`] so renderers can branch on the
/// kind of expectation that failed (text presence, count, color, style)
/// without parsing the human-readable message.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Expected {
    /// Text should be present at least once inside a region.
    TextInRegion { needle: String },
    /// Text should not appear anywhere in the frame.
    NotVisible { needle: String },
    /// Text should appear exactly `count` times in the frame.
    MatchCount { needle: String, count: usize },
    /// First match of `needle` should have the given foreground color.
    ForegroundColor { needle: String, color: CellColor },
    /// First match of `needle` should have the given background color.
    BackgroundColor { needle: String, color: CellColor },
    /// First match of `needle` should be highlighted.
    Highlighted { needle: String },
    /// First match of `needle` should not be highlighted.
    NotHighlighted { needle: String },
}

impl fmt::Display for AssertionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AssertionFailure {}

/// Match that `needle` appears at least once within `region`.
///
/// Returns `Ok(())` on success and a structured [`AssertionFailure`]
/// when the text is absent from the region.
///
/// # Errors
///
/// Returns [`Expected::TextInRegion`] when `needle` does not appear at
/// least once inside `region`.
pub fn match_text_in_region(frame: &TerminalFrame, needle: &str, region: &Region) -> MatchResult {
    let matches = frame.find_text_in_region(needle, region);

    if matches.is_empty() {
        let message = format_text_not_found(frame, needle, *region);
        let frame_excerpt = frame.text_in_region(region);

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::TextInRegion {
                needle: needle.to_string(),
            },
            region: Some(*region),
            matched_spans: frame.find_text(needle),
            frame_excerpt,
        }));
    }

    Ok(())
}

/// Assert that `needle` appears at least once within `region`.
///
/// Panic adapter for [`match_text_in_region`]. Kept for source
/// compatibility with the historical surface.
///
/// # Panics
///
/// Panics with a structured message if the text is not found in the region.
pub fn assert_text_in_region(frame: &TerminalFrame, needle: &str, region: &Region) {
    let result = match_text_in_region(frame, needle, region);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that `needle` does not appear anywhere in the terminal frame.
///
/// # Errors
///
/// Returns [`Expected::NotVisible`] when `needle` is found at least once.
pub fn match_not_visible(frame: &TerminalFrame, needle: &str) -> MatchResult {
    let matches = frame.find_text(needle);

    if !matches.is_empty() {
        let message = format!(
            "Expected text '{needle}' to NOT be visible, but found {} occurrence(s):\n{}",
            matches.len(),
            format_span_list(&matches)
        );
        let frame_excerpt = frame.all_text();

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::NotVisible {
                needle: needle.to_string(),
            },
            region: None,
            matched_spans: matches,
            frame_excerpt,
        }));
    }

    Ok(())
}

/// Assert that `needle` does not appear anywhere in the terminal frame.
///
/// Panic adapter for [`match_not_visible`].
///
/// # Panics
///
/// Panics with a structured message if the text is found.
pub fn assert_not_visible(frame: &TerminalFrame, needle: &str) {
    let result = match_not_visible(frame, needle);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that `needle` appears exactly `expected_count` times in the frame.
///
/// # Errors
///
/// Returns [`Expected::MatchCount`] when the actual count differs from
/// `expected_count`.
pub fn match_match_count(
    frame: &TerminalFrame,
    needle: &str,
    expected_count: usize,
) -> MatchResult {
    let matches = frame.find_text(needle);

    if matches.len() != expected_count {
        let message = format!(
            "Expected '{needle}' to appear {expected_count} time(s), but found {}:\n{}",
            matches.len(),
            format_span_list(&matches)
        );
        let frame_excerpt = frame.all_text();

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::MatchCount {
                needle: needle.to_string(),
                count: expected_count,
            },
            region: None,
            matched_spans: matches,
            frame_excerpt,
        }));
    }

    Ok(())
}

/// Assert that `needle` appears exactly `expected_count` times in the frame.
///
/// Uses `assert_eq!` directly so the panic output keeps the macro's
/// standard `left`/`right` block on top of the historical formatted
/// message. Downstream tests or snapshots that assert on the panic text
/// rely on that exact framing, so this adapter does not delegate to
/// [`match_match_count`].
///
/// # Panics
///
/// Panics if the count does not match.
pub fn assert_match_count(frame: &TerminalFrame, needle: &str, expected_count: usize) {
    let matches = frame.find_text(needle);
    let actual = matches.len();
    assert_eq!(
        actual,
        expected_count,
        "Expected '{needle}' to appear {expected_count} time(s), but found {actual}:\n{}",
        format_span_list(&matches)
    );
}

/// Match that the first match of `needle` has the specified foreground color.
///
/// # Errors
///
/// Returns [`Expected::ForegroundColor`] when the text is not found in
/// the frame, or when the first match's foreground color does not equal
/// `expected_color`.
pub fn match_text_has_fg_color(
    frame: &TerminalFrame,
    needle: &str,
    expected_color: &CellColor,
) -> MatchResult {
    let matches = frame.find_text(needle);

    if matches.is_empty() {
        return Err(Box::new(AssertionFailure {
            message: format!("Cannot check color: text '{needle}' not found in frame"),
            expected: Expected::ForegroundColor {
                needle: needle.to_string(),
                color: *expected_color,
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: frame.all_text(),
        }));
    }

    let span = &matches[0];

    if !span.has_fg(expected_color) {
        let message = format!(
            "Text '{needle}' at ({}, {}) has foreground {:?}, expected {:?}",
            span.rect.col, span.rect.row, span.foreground, expected_color
        );
        let frame_excerpt = frame.all_text();

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::ForegroundColor {
                needle: needle.to_string(),
                color: *expected_color,
            },
            region: None,
            matched_spans: matches,
            frame_excerpt,
        }));
    }

    Ok(())
}

/// Assert that the first match of `needle` has the specified foreground color.
///
/// Panic adapter for [`match_text_has_fg_color`].
///
/// # Panics
///
/// Panics if the text is not found or the color does not match.
pub fn assert_text_has_fg_color(frame: &TerminalFrame, needle: &str, expected_color: &CellColor) {
    let result = match_text_has_fg_color(frame, needle, expected_color);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that the first match of `needle` has the specified background color.
///
/// # Errors
///
/// Returns [`Expected::BackgroundColor`] when the text is not found in
/// the frame, or when the first match's background color does not equal
/// `expected_color`.
pub fn match_text_has_bg_color(
    frame: &TerminalFrame,
    needle: &str,
    expected_color: &CellColor,
) -> MatchResult {
    let matches = frame.find_text(needle);

    if matches.is_empty() {
        return Err(Box::new(AssertionFailure {
            message: format!("Cannot check color: text '{needle}' not found in frame"),
            expected: Expected::BackgroundColor {
                needle: needle.to_string(),
                color: *expected_color,
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: frame.all_text(),
        }));
    }

    let span = &matches[0];

    if !span.has_bg(expected_color) {
        let message = format!(
            "Text '{needle}' at ({}, {}) has background {:?}, expected {:?}",
            span.rect.col, span.rect.row, span.background, expected_color
        );
        let frame_excerpt = frame.all_text();

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::BackgroundColor {
                needle: needle.to_string(),
                color: *expected_color,
            },
            region: None,
            matched_spans: matches,
            frame_excerpt,
        }));
    }

    Ok(())
}

/// Assert that the first match of `needle` has the specified background color.
///
/// Panic adapter for [`match_text_has_bg_color`].
///
/// # Panics
///
/// Panics if the text is not found or the color does not match.
pub fn assert_text_has_bg_color(frame: &TerminalFrame, needle: &str, expected_color: &CellColor) {
    let result = match_text_has_bg_color(frame, needle, expected_color);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that the first match of `needle` is highlighted.
///
/// A span is highlighted if it is bold, inverse, or has a non-default
/// background color.
///
/// # Errors
///
/// Returns [`Expected::Highlighted`] when the text is not found in the
/// frame, or when the first match is not highlighted.
pub fn match_span_is_highlighted(frame: &TerminalFrame, needle: &str) -> MatchResult {
    let matches = frame.find_text(needle);

    if matches.is_empty() {
        return Err(Box::new(AssertionFailure {
            message: format!("Cannot check highlight: text '{needle}' not found in frame"),
            expected: Expected::Highlighted {
                needle: needle.to_string(),
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: frame.all_text(),
        }));
    }

    let span = &matches[0];

    if !span.is_highlighted() {
        let message = format!(
            "Text '{needle}' at ({}, {}) is not highlighted. Style: {:?}, fg: {:?}, bg: {:?}",
            span.rect.col, span.rect.row, span.style, span.foreground, span.background
        );
        let frame_excerpt = frame.all_text();

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::Highlighted {
                needle: needle.to_string(),
            },
            region: None,
            matched_spans: matches,
            frame_excerpt,
        }));
    }

    Ok(())
}

/// Assert that the first match of `needle` appears highlighted.
///
/// Panic adapter for [`match_span_is_highlighted`].
///
/// # Panics
///
/// Panics if the text is not found or is not highlighted.
pub fn assert_span_is_highlighted(frame: &TerminalFrame, needle: &str) {
    let result = match_span_is_highlighted(frame, needle);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that the first match of `needle` is NOT highlighted.
///
/// # Errors
///
/// Returns [`Expected::NotHighlighted`] when the text is not found in
/// the frame, or when the first match is highlighted.
pub fn match_span_is_not_highlighted(frame: &TerminalFrame, needle: &str) -> MatchResult {
    let matches = frame.find_text(needle);

    if matches.is_empty() {
        return Err(Box::new(AssertionFailure {
            message: format!("Cannot check highlight: text '{needle}' not found in frame"),
            expected: Expected::NotHighlighted {
                needle: needle.to_string(),
            },
            region: None,
            matched_spans: Vec::new(),
            frame_excerpt: frame.all_text(),
        }));
    }

    let span = &matches[0];

    if span.is_highlighted() {
        let message = format!(
            "Text '{needle}' at ({}, {}) is highlighted but should not be. Style: {:?}, fg: {:?}, \
             bg: {:?}",
            span.rect.col, span.rect.row, span.style, span.foreground, span.background
        );
        let frame_excerpt = frame.all_text();

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::NotHighlighted {
                needle: needle.to_string(),
            },
            region: None,
            matched_spans: matches,
            frame_excerpt,
        }));
    }

    Ok(())
}

/// Assert that the first match of `needle` is NOT highlighted.
///
/// Panic adapter for [`match_span_is_not_highlighted`].
///
/// # Panics
///
/// Panics if the text is not found or is highlighted.
pub fn assert_span_is_not_highlighted(frame: &TerminalFrame, needle: &str) {
    let result = match_span_is_not_highlighted(frame, needle);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Format a "text not found" error message with context.
fn format_text_not_found(frame: &TerminalFrame, needle: &str, region: Region) -> String {
    let all_matches = frame.find_text(needle);
    let region_text = frame.text_in_region(&region);
    let mut message = String::new();

    // Infallible: all `writeln!` calls below write to a String, which cannot fail.
    let _ = writeln!(
        message,
        "Text '{needle}' not found in region (col:{}, row:{}, {}x{})",
        region.col, region.row, region.width, region.height
    );

    if all_matches.is_empty() {
        let _ = writeln!(message, "  Text is not visible anywhere in the frame.");
    } else {
        let _ = writeln!(
            message,
            "  Text found {} time(s) outside the region:",
            all_matches.len()
        );
        for span in &all_matches {
            let _ = writeln!(
                message,
                "    - at (col:{}, row:{})",
                span.rect.col, span.rect.row
            );
        }
    }

    let _ = writeln!(message, "  Region content:\n{region_text}");

    message
}

/// Format a list of matched spans for error output.
fn format_span_list(spans: &[MatchedSpan]) -> String {
    let mut output = String::new();

    for span in spans {
        let _ = writeln!(
            output,
            "  - '{}' at (col:{}, row:{}) fg:{:?} bg:{:?} style:{:?}",
            span.text, span.rect.col, span.rect.row, span.foreground, span.background, span.style
        );
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assert_text_in_region_passes_when_found() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");
        let region = Region::new(0, 0, 80, 1);

        // Act / Assert — should not panic.
        assert_text_in_region(&frame, "Hello", &region);
    }

    #[test]
    #[should_panic(expected = "not found in region")]
    fn assert_text_in_region_panics_when_not_found() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");
        let region = Region::new(20, 0, 60, 1);

        // Act — should panic.
        assert_text_in_region(&frame, "Hello", &region);
    }

    #[test]
    fn match_text_in_region_returns_ok_when_found() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");
        let region = Region::new(0, 0, 80, 1);

        // Act
        let result = match_text_in_region(&frame, "Hello", &region);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_text_in_region_returns_structured_failure_when_missing() {
        // Arrange — region excludes the actual match position.
        let frame = TerminalFrame::new(80, 24, b"Hello World");
        let region = Region::new(20, 0, 60, 1);

        // Act
        let failure = match_text_in_region(&frame, "Hello", &region).expect_err("should be Err");

        // Assert
        assert_eq!(failure.region, Some(region));
        assert!(failure.message.contains("not found in region"));
        assert!(failure.frame_excerpt.is_empty() || !failure.frame_excerpt.contains("Hello"));
        assert!(
            matches!(&failure.expected, Expected::TextInRegion { needle, .. } if needle == "Hello"),
            "unexpected expected variant: {:?}",
            failure.expected
        );
        // The needle does appear elsewhere in the frame, so structured spans should
        // record it.
        assert_eq!(failure.matched_spans.len(), 1);
    }

    #[test]
    fn assert_not_visible_passes_when_absent() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");

        // Act / Assert
        assert_not_visible(&frame, "Goodbye");
    }

    #[test]
    #[should_panic(expected = "NOT be visible")]
    fn assert_not_visible_panics_when_present() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");

        // Act
        assert_not_visible(&frame, "Hello");
    }

    #[test]
    fn match_not_visible_returns_structured_failure_when_present() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello World");

        // Act
        let failure = match_not_visible(&frame, "Hello").expect_err("should be Err");

        // Assert
        assert!(failure.region.is_none());
        assert_eq!(failure.matched_spans.len(), 1);
        assert!(
            matches!(&failure.expected, Expected::NotVisible { needle, .. } if needle == "Hello"),
            "unexpected expected variant: {:?}",
            failure.expected
        );
        // frame_excerpt should contain the actual screen contents so renderers
        // have surrounding context, not just the matched span list.
        assert!(failure.frame_excerpt.contains("Hello World"));
    }

    #[test]
    fn assert_match_count_passes_with_correct_count() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"foo bar foo");

        // Act / Assert
        assert_match_count(&frame, "foo", 2);
    }

    #[test]
    #[should_panic(expected = "appear 1 time(s)")]
    fn assert_match_count_panics_with_wrong_count() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"foo bar foo");

        // Act
        assert_match_count(&frame, "foo", 1);
    }

    #[test]
    fn match_match_count_returns_ok_for_correct_count() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"foo bar foo");

        // Act
        let result = match_match_count(&frame, "foo", 2);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_match_count_returns_failure_for_wrong_count() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"foo bar foo");

        // Act
        let failure = match_match_count(&frame, "foo", 1).expect_err("should be Err");

        // Assert
        assert!(
            matches!(
                &failure.expected,
                Expected::MatchCount { needle, count, .. } if needle == "foo" && *count == 1
            ),
            "unexpected expected variant: {:?}",
            failure.expected
        );
        assert_eq!(failure.matched_spans.len(), 2);
    }

    #[test]
    fn assert_span_is_highlighted_detects_bold() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"\x1b[1mBold\x1b[0m");

        // Act / Assert
        assert_span_is_highlighted(&frame, "Bold");
    }

    #[test]
    fn match_span_is_highlighted_returns_failure_when_missing() {
        // Arrange — text is not on screen.
        let frame = TerminalFrame::new(80, 24, b"plain text");

        // Act
        let failure = match_span_is_highlighted(&frame, "missing").expect_err("should be Err");

        // Assert
        assert!(failure.matched_spans.is_empty());
        assert!(
            matches!(&failure.expected, Expected::Highlighted { needle, .. } if needle == "missing"),
            "unexpected expected variant: {:?}",
            failure.expected
        );
        // frame_excerpt should include the visible screen contents even on
        // missing-text failures.
        assert!(failure.frame_excerpt.contains("plain text"));
    }

    #[test]
    fn match_span_is_highlighted_returns_failure_for_plain_text() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"plain text");

        // Act
        let failure = match_span_is_highlighted(&frame, "plain").expect_err("should be Err");

        // Assert
        assert!(
            matches!(&failure.expected, Expected::Highlighted { needle, .. } if needle == "plain"),
            "unexpected expected variant: {:?}",
            failure.expected
        );
        assert_eq!(failure.matched_spans.len(), 1);
    }

    #[test]
    fn assert_span_is_not_highlighted_for_plain_text() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"plain text");

        // Act / Assert
        assert_span_is_not_highlighted(&frame, "plain");
    }

    #[test]
    fn match_span_is_not_highlighted_returns_failure_for_bold() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"\x1b[1mBold\x1b[0m");

        // Act
        let failure = match_span_is_not_highlighted(&frame, "Bold").expect_err("should be Err");

        // Assert
        assert!(
            matches!(&failure.expected, Expected::NotHighlighted { needle, .. } if needle == "Bold"),
            "unexpected expected variant: {:?}",
            failure.expected
        );
    }

    #[test]
    fn assert_text_has_fg_color_passes() {
        // Arrange — ANSI red foreground.
        let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

        // Act / Assert
        assert_text_has_fg_color(&frame, "Red", &CellColor::new(128, 0, 0));
    }

    #[test]
    #[should_panic(expected = "foreground")]
    fn assert_text_has_fg_color_panics_on_mismatch() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

        // Act
        assert_text_has_fg_color(&frame, "Red", &CellColor::new(0, 255, 0));
    }

    #[test]
    fn match_text_has_fg_color_returns_failure_when_text_missing() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"plain");

        // Act
        let failure = match_text_has_fg_color(&frame, "missing", &CellColor::new(0, 255, 0))
            .expect_err("should be Err");

        // Assert
        assert!(failure.message.contains("not found in frame"));
        assert!(failure.matched_spans.is_empty());
        // frame_excerpt should show what is on screen so renderers can surface
        // surrounding context for the missing-text failure.
        assert!(failure.frame_excerpt.contains("plain"));
    }

    #[test]
    fn match_text_has_fg_color_returns_failure_on_mismatch() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

        // Act
        let failure = match_text_has_fg_color(&frame, "Red", &CellColor::new(0, 255, 0))
            .expect_err("should be Err");

        // Assert
        assert!(
            matches!(
                &failure.expected,
                Expected::ForegroundColor { needle, color, .. }
                    if needle == "Red" && *color == CellColor::new(0, 255, 0)
            ),
            "unexpected expected variant: {:?}",
            failure.expected
        );
        assert_eq!(failure.matched_spans.len(), 1);
    }

    #[test]
    fn match_text_has_bg_color_returns_failure_when_text_missing() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"plain");

        // Act
        let failure = match_text_has_bg_color(&frame, "missing", &CellColor::new(0, 0, 0))
            .expect_err("should be Err");

        // Assert
        assert!(failure.message.contains("not found in frame"));
        assert!(failure.matched_spans.is_empty());
        // frame_excerpt should show what is on screen so renderers can surface
        // surrounding context for the missing-text failure.
        assert!(failure.frame_excerpt.contains("plain"));
    }

    #[test]
    fn match_text_has_bg_color_returns_ok_when_match() {
        // Arrange — ANSI red background (index 41 → CellColor 128,0,0).
        let frame = TerminalFrame::new(80, 24, b"\x1b[41mActive\x1b[0m");

        // Act
        let result = match_text_has_bg_color(&frame, "Active", &CellColor::new(128, 0, 0));

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_text_has_bg_color_returns_failure_on_mismatch() {
        // Arrange — red background, but caller expects a different color.
        let frame = TerminalFrame::new(80, 24, b"\x1b[41mActive\x1b[0m");

        // Act
        let failure = match_text_has_bg_color(&frame, "Active", &CellColor::new(0, 0, 128))
            .expect_err("should be Err");

        // Assert
        assert!(
            matches!(
                &failure.expected,
                Expected::BackgroundColor { needle, color, .. }
                    if needle == "Active" && *color == CellColor::new(0, 0, 128)
            ),
            "unexpected expected variant: {:?}",
            failure.expected
        );
        assert_eq!(failure.matched_spans.len(), 1);
        // frame_excerpt should reflect on-screen content so renderers can show
        // the surrounding context for the mismatch.
        assert!(failure.frame_excerpt.contains("Active"));
    }

    #[test]
    fn match_span_is_not_highlighted_returns_failure_when_missing() {
        // Arrange — text is not on screen at all.
        let frame = TerminalFrame::new(80, 24, b"plain text");

        // Act
        let failure = match_span_is_not_highlighted(&frame, "missing").expect_err("should be Err");

        // Assert
        assert!(failure.matched_spans.is_empty());
        assert!(failure.message.contains("not found in frame"));
        assert!(
            matches!(&failure.expected, Expected::NotHighlighted { needle, .. } if needle == "missing"),
            "unexpected expected variant: {:?}",
            failure.expected
        );
        // frame_excerpt should include the visible screen contents even on
        // missing-text failures.
        assert!(failure.frame_excerpt.contains("plain text"));
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
}
