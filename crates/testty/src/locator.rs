//! Text locators for finding and describing UI elements in the terminal grid.
//!
//! A [`MatchedSpan`] describes a contiguous run of styled text found at a
//! specific position in the terminal. Locators combine text search with
//! style and color filtering to identify TUI "controls" such as tabs,
//! buttons, and highlighted labels.

use crate::frame::{CellColor, CellStyle};
use crate::region::Region;

/// A matched span of text found in the terminal grid.
///
/// Contains the text content, its bounding rectangle, and the style and
/// color information extracted from the first cell. Spans are always
/// single-row since terminal text does not wrap across rows for matching
/// purposes.
#[derive(Debug, Clone)]
pub struct MatchedSpan {
    /// Background color of the first cell, or `None` for terminal default.
    pub background: Option<CellColor>,
    /// Foreground color of the first cell, or `None` for terminal default.
    pub foreground: Option<CellColor>,
    /// The bounding rectangle in terminal cell coordinates.
    pub rect: Region,
    /// Style flags of the first cell.
    pub style: CellStyle,
    /// The text content of the matched span.
    pub text: String,
}

impl MatchedSpan {
    /// Check whether this span has a specific foreground color.
    pub fn has_fg(&self, color: &CellColor) -> bool {
        self.foreground.as_ref() == Some(color)
    }

    /// Check whether this span has a specific background color.
    pub fn has_bg(&self, color: &CellColor) -> bool {
        self.background.as_ref() == Some(color)
    }

    /// Check whether this span is rendered bold.
    pub fn is_bold(&self) -> bool {
        self.style.bold()
    }

    /// Check whether this span is rendered with inverse colors.
    pub fn is_inverse(&self) -> bool {
        self.style.inverse()
    }

    /// Check whether this span appears visually highlighted.
    ///
    /// A span is considered highlighted if it is bold, inverse, or has a
    /// non-default background color.
    pub fn is_highlighted(&self) -> bool {
        self.style.bold() || self.style.inverse() || self.background.is_some()
    }
}

#[cfg(test)]
#[path = "locator_test.rs"]
mod tests;
