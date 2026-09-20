//! Per-frame recorder for scrollable panel geometry.
//!
//! Pages call the `record_*` helpers while painting so the runtime can hit-test
//! mouse coordinates against the exact rectangles the last frame used. The
//! recorder is thread-local frame scratch state, mirroring the scoped active
//! theme in `style.rs`: `render_app()` clears it before routing the frame and
//! takes the finished snapshot afterwards, so no render context needs to
//! thread a snapshot handle through every page constructor.

use std::cell::RefCell;

use ratatui::layout::Rect;

use crate::presentation::app_mode::ViewportRect;
use crate::presentation::viewport::{LayoutSnapshot, ScrollRegion};

thread_local! {
    static FRAME_LAYOUT: RefCell<LayoutSnapshot> = RefCell::new(LayoutSnapshot::default());
}

/// Clears recorded regions before a new frame is routed.
pub(crate) fn begin_frame() {
    FRAME_LAYOUT.with(|layout| *layout.borrow_mut() = LayoutSnapshot::default());
}

/// Returns the regions recorded since [`begin_frame`], leaving the recorder
/// empty.
pub(crate) fn take_frame() -> LayoutSnapshot {
    FRAME_LAYOUT.with(|layout| std::mem::take(&mut *layout.borrow_mut()))
}

/// Records the session transcript panel painted by the current frame.
pub(crate) fn record_chat_output(region: ScrollRegion) {
    FRAME_LAYOUT.with(|layout| layout.borrow_mut().chat_output = Some(region));
}

/// Records the diff-mode file explorer column painted by the current frame.
pub(crate) fn record_diff_file_list(area: Rect) {
    FRAME_LAYOUT.with(|layout| layout.borrow_mut().diff_file_list = Some(viewport_rect(area)));
}

/// Records the diff-mode right panel painted by the current frame.
pub(crate) fn record_diff_panel(region: ScrollRegion) {
    FRAME_LAYOUT.with(|layout| layout.borrow_mut().diff_panel = Some(region));
}

/// Records the help popup painted by the current frame.
pub(crate) fn record_help_overlay(region: ScrollRegion) {
    FRAME_LAYOUT.with(|layout| layout.borrow_mut().help_overlay = Some(region));
}

/// Builds a scroll region from Ratatui geometry and rendered content metrics.
pub(crate) fn scroll_region(
    area: Rect,
    scrollbar: Option<Rect>,
    total_lines: usize,
    viewport_height: u16,
) -> ScrollRegion {
    ScrollRegion {
        area: viewport_rect(area),
        scrollbar: scrollbar.map(viewport_rect),
        total_lines: u16::try_from(total_lines).unwrap_or(u16::MAX),
        viewport_height,
    }
}

/// Converts Ratatui geometry into the frontend-neutral rectangle contract.
pub(crate) fn viewport_rect(area: Rect) -> ViewportRect {
    ViewportRect {
        height: area.height,
        width: area.width,
        x: area.x,
        y: area.y,
    }
}

#[cfg(test)]
#[path = "layout_snapshot_test.rs"]
mod tests;
