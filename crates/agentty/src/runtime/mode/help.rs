use crossterm::event::{KeyCode, KeyEvent};

use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;

/// Handles key input while the app is showing the help overlay.
pub(crate) fn handle(mode: &mut AppMode, key: KeyEvent) -> EventResult {
    if let AppMode::Help {
        scroll_offset,
        context: _,
    } = mode
    {
        match key.code {
            KeyCode::Char('?' | 'q') | KeyCode::Esc => {
                let previous_mode = std::mem::replace(mode, AppMode::List);
                if let AppMode::Help { context, .. } = previous_mode {
                    *mode = context.restore_mode();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                *scroll_offset = scroll_offset.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *scroll_offset = scroll_offset.saturating_sub(1);
            }
            _ => {}
        }
    }

    EventResult::Continue
}

#[cfg(test)]
#[path = "help_test.rs"]
mod tests;
