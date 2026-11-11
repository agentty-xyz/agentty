use crossterm::event::{KeyCode, KeyEvent};

use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;

/// Handles key input while the app is showing the help overlay.
///
/// Any other mode is left unchanged.
pub(crate) fn handle(mode: &mut AppMode, key: KeyEvent) -> EventResult {
    *mode = match (std::mem::replace(mode, AppMode::List), key.code) {
        (AppMode::Help { context, .. }, KeyCode::Char('?' | 'q') | KeyCode::Esc) => {
            context.restore_mode()
        }
        (
            AppMode::Help {
                context,
                scroll_offset,
            },
            KeyCode::Char('j') | KeyCode::Down,
        ) => AppMode::Help {
            context,
            scroll_offset: scroll_offset.saturating_add(1),
        },
        (
            AppMode::Help {
                context,
                scroll_offset,
            },
            KeyCode::Char('k') | KeyCode::Up,
        ) => AppMode::Help {
            context,
            scroll_offset: scroll_offset.saturating_sub(1),
        },
        (previous_mode, _) => previous_mode,
    };

    EventResult::Continue
}

#[cfg(test)]
#[path = "help_test.rs"]
mod tests;
