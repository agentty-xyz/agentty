//! Frontend-neutral scroll-region geometry recorded by one rendered frame.
//!
//! The render pass records where each scrollable panel landed, plus the
//! content height it painted, so mouse input can hit-test pointer coordinates
//! and clamp scroll offsets without recomputing layout. Keeping these types
//! free of Ratatui lets runtime input and UI output share them.

use crate::presentation::app_mode::ViewportRect;

/// Number of content lines one mouse wheel notch scrolls.
pub const MOUSE_WHEEL_SCROLL_LINES: u16 = 3;

/// One scrollable panel as painted by the most recent frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScrollRegion {
    /// Panel rectangle including borders; pointer hit-tests use this.
    pub area: ViewportRect,
    /// One-column scrollbar track rectangle when the panel drew a scrollbar.
    pub scrollbar: Option<ViewportRect>,
    /// Total rendered content lines for the panel.
    pub total_lines: u16,
    /// Visible content rows inside the panel borders.
    pub viewport_height: u16,
}

impl ScrollRegion {
    /// Returns whether the pointer position lies inside the panel area.
    #[must_use]
    pub fn contains(&self, column: u16, row: u16) -> bool {
        self.area.contains(column, row)
    }

    /// Returns whether the pointer position lies on the scrollbar track.
    #[must_use]
    pub fn scrollbar_contains(&self, column: u16, row: u16) -> bool {
        self.scrollbar
            .is_some_and(|scrollbar| scrollbar.contains(column, row))
    }

    /// Returns the largest top-line offset that still fills the viewport.
    #[must_use]
    pub fn max_scroll_offset(&self) -> u16 {
        self.total_lines.saturating_sub(self.viewport_height)
    }

    /// Scrolls a clamped top-line offset down by `step` lines.
    #[must_use]
    pub fn scroll_down(&self, scroll_offset: u16, step: u16) -> u16 {
        scroll_offset
            .min(self.max_scroll_offset())
            .saturating_add(step.max(1))
            .min(self.max_scroll_offset())
    }

    /// Scrolls a clamped top-line offset up by `step` lines.
    #[must_use]
    pub fn scroll_up(&self, scroll_offset: u16, step: u16) -> u16 {
        scroll_offset
            .min(self.max_scroll_offset())
            .saturating_sub(step.max(1))
    }

    /// Scrolls a follow-tail chat offset down by `step` lines.
    ///
    /// `None` means the panel follows the newest output. Scrolling past the
    /// last line re-enters follow-tail mode so live output keeps streaming
    /// into view, matching the `j`/`Ctrl+d` key semantics.
    #[must_use]
    pub fn scroll_tail_down(&self, scroll_offset: Option<u16>, step: u16) -> Option<u16> {
        let current_offset = scroll_offset?;
        let next_offset = current_offset.saturating_add(step.max(1));
        if next_offset >= self.max_scroll_offset() {
            return None;
        }

        Some(next_offset)
    }

    /// Scrolls a follow-tail chat offset up by `step` lines, pinning the view.
    #[must_use]
    pub fn scroll_tail_up(&self, scroll_offset: Option<u16>, step: u16) -> u16 {
        scroll_offset
            .unwrap_or_else(|| self.max_scroll_offset())
            .saturating_sub(step.max(1))
    }

    /// Converts a pointer row on the scrollbar track into a top-line offset.
    ///
    /// `grab_row_within_thumb` is the row offset inside the thumb where the
    /// drag started, so the thumb stays anchored under the pointer instead of
    /// jumping to center on it.
    #[must_use]
    pub fn scroll_offset_for_pointer_row(
        &self,
        pointer_row: u16,
        grab_row_within_thumb: u16,
    ) -> u16 {
        let Some(scrollbar) = self.scrollbar else {
            return 0;
        };
        let geometry = ScrollbarGeometry::new(scrollbar.height, self.total_lines, 0);
        let thumb_row = pointer_row
            .saturating_sub(scrollbar.y)
            .saturating_sub(grab_row_within_thumb);

        geometry.scroll_offset_for_thumb_row(thumb_row)
    }

    /// Returns the row offset inside the thumb for a pointer press on the
    /// track, or `None` when the press landed outside the thumb.
    #[must_use]
    pub fn thumb_grab_row(&self, scroll_offset: u16, pointer_row: u16) -> Option<u16> {
        let scrollbar = self.scrollbar?;
        let geometry = ScrollbarGeometry::new(scrollbar.height, self.total_lines, scroll_offset);
        let track_row = pointer_row.checked_sub(scrollbar.y)?;
        if !geometry.is_thumb_row(track_row) {
            return None;
        }

        Some(track_row - geometry.thumb_offset)
    }
}

/// Thumb placement for one vertical scrollbar track.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollbarGeometry {
    /// Rows the thumb occupies; always at least one when the track is visible.
    pub thumb_height: u16,
    /// Row inside the track where the thumb starts.
    pub thumb_offset: u16,
    total_lines: u16,
    track_height: u16,
}

impl ScrollbarGeometry {
    /// Computes thumb size and position for a track showing `total_lines`
    /// scrolled to `scroll_offset`.
    #[must_use]
    pub fn new(track_height: u16, total_lines: u16, scroll_offset: u16) -> Self {
        let track = usize::from(track_height);
        let total = usize::from(total_lines).max(1);
        let thumb_height = (track * track / total).clamp(1, track.max(1));
        let max_scroll = total.saturating_sub(track);
        let max_thumb_offset = track.saturating_sub(thumb_height);
        let thumb_offset = usize::from(scroll_offset)
            .min(max_scroll)
            .saturating_mul(max_thumb_offset)
            .checked_div(max_scroll)
            .unwrap_or(0);

        Self {
            thumb_height: u16::try_from(thumb_height).unwrap_or(u16::MAX),
            thumb_offset: u16::try_from(thumb_offset).unwrap_or(u16::MAX),
            total_lines,
            track_height,
        }
    }

    /// Returns whether `track_row` is covered by the thumb.
    #[must_use]
    pub fn is_thumb_row(self, track_row: u16) -> bool {
        track_row >= self.thumb_offset
            && track_row < self.thumb_offset.saturating_add(self.thumb_height)
    }

    /// Inverse of [`ScrollbarGeometry::new`]: maps the thumb's top row back to
    /// the top-line scroll offset, clamped to the scrollable range.
    #[must_use]
    pub fn scroll_offset_for_thumb_row(self, thumb_row: u16) -> u16 {
        let track = usize::from(self.track_height);
        let total = usize::from(self.total_lines);
        let max_scroll = total.saturating_sub(track);
        let max_thumb_offset = track.saturating_sub(usize::from(self.thumb_height));
        if max_thumb_offset == 0 {
            return 0;
        }

        let scroll_offset = usize::from(thumb_row)
            .min(max_thumb_offset)
            .saturating_mul(max_scroll)
            .div_ceil(max_thumb_offset)
            .min(max_scroll);

        u16::try_from(scroll_offset).unwrap_or(u16::MAX)
    }
}

/// Scrollable panels recorded by the most recently rendered frame.
///
/// The frame render resets this snapshot, pages record the regions they paint,
/// and the runtime keeps the result until the next frame replaces it. Absent
/// regions mean the panel was not on screen, so mouse input over it is
/// ignored.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LayoutSnapshot {
    /// Session transcript panel shown in view, prompt, and question modes.
    pub chat_output: Option<ScrollRegion>,
    /// Diff-mode file explorer column.
    pub diff_file_list: Option<ViewportRect>,
    /// Diff-mode right panel (diff or comments).
    pub diff_panel: Option<ScrollRegion>,
    /// Keybinding help popup.
    pub help_overlay: Option<ScrollRegion>,
}

/// Identifies which recorded scroll region a scrollbar drag is attached to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollRegionKind {
    /// Session transcript panel.
    ChatOutput,
    /// Diff-mode right panel.
    DiffPanel,
}

/// In-progress scrollbar drag started by a left-button press on a thumb.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollbarDrag {
    /// Row offset inside the thumb where the pointer grabbed it.
    pub grab_row_within_thumb: u16,
    /// Region whose scrollbar is being dragged.
    pub region: ScrollRegionKind,
}

#[cfg(test)]
#[path = "viewport_test.rs"]
mod tests;
