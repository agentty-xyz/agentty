//! Agent-friendly recipe helpers for common TUI assertions.
//!
//! Provides a small set of composable, high-level helpers that wrap raw
//! locators and region checks. These helpers are designed so that AI agents
//! and contributors can write feature-oriented regression tests without
//! rebuilding locator and color logic from scratch.
//!
//! # Layered API
//!
//! Each recipe is exposed in two layers, mirroring the
//! [`crate::assertion`] module:
//!
//! - **`match_*` recipes** return [`MatchResult`] so callers can compose,
//!   retry, accumulate failures via [`crate::assertion::SoftAssertions`], or
//!   surface the failure into a [`crate::proof::report::ProofReport`] without
//!   unwinding.
//! - **`expect_*` recipes** are thin panic adapters that delegate to the
//!   matching `match_*` and panic with the structured failure message. They are
//!   kept for source compatibility with tests that expect panic-on-failure
//!   semantics.
//!
//! # Vocabulary
//!
//! - **Tab**: A labeled text span in the header row, where the selected tab
//!   appears highlighted (bold, inverse, or non-default background).
//! - **Instruction**: Visible help text or description in a designated area.
//! - **Keybinding hint**: A compact label like `Tab`, `Enter`, `q` in the
//!   footer area describing available actions.
//! - **Footer action**: A labeled action in the bottom row.
//! - **Dialog title**: A centered heading in a modal-like area.
//! - **Status message**: A transient notification in a specific region.

use crate::assertion;
use crate::assertion::{AssertionFailure, Expected, MatchResult};
use crate::frame::TerminalFrame;
use crate::region::Region;

/// Match that a tab with the given label exists in the header row and
/// appears highlighted (selected).
///
/// First proves the label exists in the header row via
/// [`assertion::match_text_in_region`], then inspects the highlight state
/// of that header occurrence specifically. The highlight check is scoped
/// to the header region so that a duplicate of the same label rendered
/// elsewhere in the frame cannot mask or fabricate a tab-state failure.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// label is missing from the header row or the header occurrence is not
/// highlighted.
pub fn match_selected_tab(frame: &TerminalFrame, label: &str) -> MatchResult {
    let header = Region::top_row(frame.cols());
    assertion::match_text_in_region(frame, label, &header)?;

    let header_matches = frame.find_text_in_region(label, &header);
    let span = &header_matches[0];

    if !span.is_highlighted() {
        let message = format!(
            "Text '{label}' at ({}, {}) is not highlighted. Style: {:?}, fg: {:?}, bg: {:?}",
            span.rect.col, span.rect.row, span.style, span.foreground, span.background
        );

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::Highlighted {
                needle: label.to_string(),
            },
            region: Some(header),
            matched_spans: header_matches,
            frame_excerpt: frame.all_text(),
        }));
    }

    Ok(())
}

/// Assert that a tab with the given label exists in the header row
/// and appears highlighted (selected).
///
/// Panic adapter for [`match_selected_tab`].
///
/// # Panics
///
/// Panics if the tab label is not found in the top row or is not highlighted.
pub fn expect_selected_tab(frame: &TerminalFrame, label: &str) {
    let result = match_selected_tab(frame, label);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a tab with the given label exists in the header row but is
/// NOT highlighted (not selected).
///
/// First proves the label exists in the header row via
/// [`assertion::match_text_in_region`], then inspects the highlight state
/// of that header occurrence specifically. The highlight check is scoped
/// to the header region so that a duplicate of the same label rendered
/// elsewhere in the frame cannot mask or fabricate a tab-state failure.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// label is missing from the header row or the header occurrence is
/// highlighted.
pub fn match_unselected_tab(frame: &TerminalFrame, label: &str) -> MatchResult {
    let header = Region::top_row(frame.cols());
    assertion::match_text_in_region(frame, label, &header)?;

    let header_matches = frame.find_text_in_region(label, &header);
    let span = &header_matches[0];

    if span.is_highlighted() {
        let message = format!(
            "Text '{label}' at ({}, {}) is highlighted but should not be. Style: {:?}, fg: {:?}, \
             bg: {:?}",
            span.rect.col, span.rect.row, span.style, span.foreground, span.background
        );

        return Err(Box::new(AssertionFailure {
            message,
            expected: Expected::NotHighlighted {
                needle: label.to_string(),
            },
            region: Some(header),
            matched_spans: header_matches,
            frame_excerpt: frame.all_text(),
        }));
    }

    Ok(())
}

/// Assert that a tab with the given label exists in the header row
/// but is NOT highlighted (not selected).
///
/// Panic adapter for [`match_unselected_tab`].
///
/// # Panics
///
/// Panics if the tab label is not found or is highlighted.
pub fn expect_unselected_tab(frame: &TerminalFrame, label: &str) {
    let result = match_unselected_tab(frame, label);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that an instruction or help text is visible in the frame.
///
/// Checks the full terminal grid since instructions may appear in
/// different areas depending on the application state.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// instruction text is not present anywhere in the frame.
pub fn match_instruction_visible(frame: &TerminalFrame, instruction: &str) -> MatchResult {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, instruction, &full)
}

/// Assert that an instruction or help text is visible in the frame.
///
/// Panic adapter for [`match_instruction_visible`].
///
/// # Panics
///
/// Panics if the instruction text is not found.
pub fn expect_instruction_visible(frame: &TerminalFrame, instruction: &str) {
    let result = match_instruction_visible(frame, instruction);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a keybinding hint appears in the footer row.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// hint is missing from the footer row.
pub fn match_keybinding_hint(frame: &TerminalFrame, hint: &str) -> MatchResult {
    let footer = Region::footer(frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, hint, &footer)
}

/// Assert that a keybinding hint appears in the footer row.
///
/// Panic adapter for [`match_keybinding_hint`].
///
/// # Panics
///
/// Panics if the hint text is not found in the footer.
pub fn expect_keybinding_hint(frame: &TerminalFrame, hint: &str) {
    let result = match_keybinding_hint(frame, hint);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a labeled action appears in the footer row.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// action label is missing from the footer row.
pub fn match_footer_action(frame: &TerminalFrame, action: &str) -> MatchResult {
    let footer = Region::footer(frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, action, &footer)
}

/// Assert that a labeled action appears in the footer row.
///
/// Panic adapter for [`match_footer_action`].
///
/// # Panics
///
/// Panics if the action label is not found in the footer.
pub fn expect_footer_action(frame: &TerminalFrame, action: &str) {
    let result = match_footer_action(frame, action);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a dialog title appears in the terminal.
///
/// Searches the upper portion of the terminal (top 60%) since dialogs
/// typically render as centered overlays.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// title text is not present in the upper 60% of the frame.
pub fn match_dialog_title(frame: &TerminalFrame, title: &str) -> MatchResult {
    let upper = Region::percent(0, 0, 100, 60, frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, title, &upper)
}

/// Assert that a dialog title appears in the terminal.
///
/// Panic adapter for [`match_dialog_title`].
///
/// # Panics
///
/// Panics if the title text is not found.
pub fn expect_dialog_title(frame: &TerminalFrame, title: &str) {
    let result = match_dialog_title(frame, title);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a status message is visible anywhere in the frame.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// message text is not present anywhere in the frame.
pub fn match_status_message(frame: &TerminalFrame, message: &str) -> MatchResult {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::match_text_in_region(frame, message, &full)
}

/// Assert that a status message is visible anywhere in the frame.
///
/// Panic adapter for [`match_status_message`].
///
/// # Panics
///
/// Panics if the status message is not found.
pub fn expect_status_message(frame: &TerminalFrame, message: &str) {
    let result = match_status_message(frame, message);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

/// Match that a specific text is NOT visible anywhere in the frame.
///
/// # Errors
///
/// Returns the underlying [`crate::assertion::AssertionFailure`] when the
/// text appears at least once in the frame.
pub fn match_not_visible(frame: &TerminalFrame, text: &str) -> MatchResult {
    assertion::match_not_visible(frame, text)
}

/// Assert that a specific text is NOT visible anywhere in the frame.
///
/// Panic adapter for [`match_not_visible`].
///
/// # Panics
///
/// Panics if the text is found.
pub fn expect_not_visible(frame: &TerminalFrame, text: &str) {
    let result = match_not_visible(frame, text);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|f| f.message).unwrap_or_default()
    );
}

#[cfg(test)]
#[path = "recipe_test.rs"]
mod tests;
