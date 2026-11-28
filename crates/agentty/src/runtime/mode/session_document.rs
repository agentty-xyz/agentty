//! Key handling for the session notes panel.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use super::input_key::{self, InputCapabilities};
use crate::app::App;
use crate::domain::input::InputState;
use crate::presentation::app_mode::AppMode;
use crate::presentation::session_document::{
    CLEAR_CONFIRMATION_WORD, ClearConfirmation, SessionDocumentPanel,
};
use crate::runtime::EventResult;
use crate::ui::component::session_document_overlay::{self, SessionDocumentLayoutCache};

/// Lines that `PageUp` and `PageDown` scroll the panel by.
const PAGE_SCROLL_LINES: u16 = 10;

/// Handles key input while the notes panel is open.
///
/// While the clearing confirmation is open, text keys edit it, `Enter`
/// clears the document once it reads [`CLEAR_CONFIRMATION_WORD`], and `Esc`
/// closes it. Otherwise `Esc` and `q` restore the session view or graph
/// inspector that opened the panel, `x` opens the confirmation when there
/// is something to clear, `j`/`k` and the arrow keys scroll by one line,
/// and `PageDown`/`PageUp` by a page, never past `max_scroll_offset`.
/// Other keys and other modes are left unchanged.
pub(crate) async fn handle(app: &mut App, max_scroll_offset: u16, key: KeyEvent) -> EventResult {
    let AppMode::SessionDocument(panel) = &mut app.mode else {
        return EventResult::Continue;
    };
    match resolve_key(panel, max_scroll_offset, key) {
        PanelAction::Clear(version) => app.clear_session_document(version).await,
        PanelAction::Close => app.close_session_document(),
        PanelAction::None => {}
    }

    EventResult::Continue
}

/// What a key asks of the app beyond the panel's own state.
enum PanelAction {
    /// Clear the document at the version the typed confirmation opened on.
    Clear(u64),
    /// Close the panel and restore the page that opened it.
    Close,
    /// Nothing beyond the panel's own state.
    None,
}

/// Applies `key` to the panel's own state and returns what else it asks.
fn resolve_key(
    panel: &mut SessionDocumentPanel,
    max_scroll_offset: u16,
    key: KeyEvent,
) -> PanelAction {
    if let Some(confirmation) = &mut panel.clear_confirmation {
        if key.code == KeyCode::Esc {
            panel.clear_confirmation = None;
        } else if input_key::is_enter_key(key.code) {
            if confirmation.input.text().trim() == CLEAR_CONFIRMATION_WORD {
                return PanelAction::Clear(confirmation.version);
            }
            panel.notice = Some(format!(
                "Type {CLEAR_CONFIRMATION_WORD} to confirm clearing the session notes."
            ));
        } else if let Some(command) =
            input_key::command_for_key(key, InputCapabilities::SINGLE_LINE)
        {
            confirmation.input.apply(command);
        }

        return PanelAction::None;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return PanelAction::Close,
        KeyCode::Char('x') => {
            if let Some(version) = panel.clearable_version() {
                panel.notice = None;
                panel.clear_confirmation = Some(ClearConfirmation {
                    input: InputState::default(),
                    version,
                });
            }
        }
        code => {
            panel.scroll_offset = scrolled_offset(panel.scroll_offset, code).min(max_scroll_offset);
        }
    }

    PanelAction::None
}

/// Returns how far the open notes panel can scroll when drawn in
/// `content_area`, laid out exactly as the overlay draws it through
/// `cache`; zero when no panel is open.
pub(crate) fn max_scroll_offset(
    app: &App,
    cache: &SessionDocumentLayoutCache,
    content_area: Rect,
) -> u16 {
    let AppMode::SessionDocument(panel) = &app.mode else {
        return 0;
    };

    session_document_overlay::max_scroll_offset(panel, cache, content_area)
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
#[path = "session_document_test.rs"]
mod tests;
