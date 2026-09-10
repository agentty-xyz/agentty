use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;

/// Handles key input while the app is showing the help overlay.
pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    if let AppMode::Help {
        scroll_offset,
        context: _,
    } = &mut app.mode
    {
        match key.code {
            KeyCode::Char('?' | 'q') | KeyCode::Esc => {
                let mode = std::mem::replace(&mut app.mode, AppMode::List);
                if let AppMode::Help { context, .. } = mode {
                    app.mode = context.restore_mode();
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
