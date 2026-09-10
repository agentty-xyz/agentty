use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;

/// Handles key input while the sync informational popup is visible.
pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::SyncBlockedPopup { is_loading, .. } = &app.mode else {
        return EventResult::Continue;
    };

    if *is_loading {
        return EventResult::Continue;
    }

    if should_close_sync_blocked_popup(key) {
        app.mode = AppMode::List;
    }

    EventResult::Continue
}

/// Returns whether the pressed key should close the sync informational popup.
fn should_close_sync_blocked_popup(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter | KeyCode::Esc => true,
        KeyCode::Char(character) => character.eq_ignore_ascii_case(&'q'),
        _ => false,
    }
}

#[cfg(test)]
#[path = "sync_blocked_test.rs"]
mod tests;
