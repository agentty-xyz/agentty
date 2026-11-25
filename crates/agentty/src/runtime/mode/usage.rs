//! Key handling for the per-session usage panel.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;
use crate::ui::component::usage_overlay;
use crate::ui::usage_format;

/// Lines that `PageUp` and `PageDown` scroll the panel by.
const PAGE_SCROLL_LINES: u16 = 10;

/// Handles key input while the usage panel is open: `Esc` restores the
/// session page that opened it, `j`/`k` and the arrow keys scroll by one
/// line, `PageDown`/`PageUp` by a page, never past `max_scroll_offset`, and
/// other keys are ignored.
///
/// Any other mode is left unchanged.
pub(crate) fn handle(mode: &mut AppMode, max_scroll_offset: u16, key: KeyEvent) -> EventResult {
    *mode = match (std::mem::replace(mode, AppMode::List), key.code) {
        (AppMode::Usage(panel), KeyCode::Esc) => panel.restore.into_mode(),
        (AppMode::Usage(mut panel), code) => {
            panel.scroll_offset = scrolled_offset(panel.scroll_offset, code).min(max_scroll_offset);

            AppMode::Usage(panel)
        }
        (previous_mode, _) => previous_mode,
    };

    EventResult::Continue
}

/// Returns how far the open usage panel can scroll when drawn in
/// `content_area`, laid out exactly as the overlay draws it; zero when no
/// panel is open.
pub(crate) fn max_scroll_offset(app: &App, content_area: Rect) -> u16 {
    let AppMode::Usage(panel) = &app.mode else {
        return 0;
    };
    let content = usage_format::session_panel_content(
        &panel.usage,
        panel.restore.session_id(),
        &app.session_live_usage,
    );

    usage_overlay::max_scroll_offset(&content, content_area)
}

/// Returns the scroll offset after `code`, before the caller bounds it.
fn scrolled_offset(scroll_offset: u16, code: KeyCode) -> u16 {
    match code {
        KeyCode::Char('j') | KeyCode::Down => scroll_offset.saturating_add(1),
        KeyCode::Char('k') | KeyCode::Up => scroll_offset.saturating_sub(1),
        KeyCode::PageDown => scroll_offset.saturating_add(PAGE_SCROLL_LINES),
        KeyCode::PageUp => scroll_offset.saturating_sub(PAGE_SCROLL_LINES),
        _ => scroll_offset,
    }
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;
