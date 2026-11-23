//! Key and paste handling for the harness provider API key popup.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::domain::input::InputCommand;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;
use crate::runtime::mode::input_key;

/// Handles key input while the API key popup is visible.
///
/// While editing, `Enter` saves the key, `Esc` cancels, and other keys edit
/// the single-line field. After the key is saved, `Enter` or `Esc` closes
/// the popup.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::ApiKeyInput { dialog } = &mut app.mode else {
        return EventResult::Continue;
    };

    match key.code {
        KeyCode::Enter if dialog.is_editing() => app.save_api_key().await,
        KeyCode::Esc | KeyCode::Enter => app.close_api_key_input(),
        _ if dialog.is_editing() => {
            if let Some(command) =
                input_key::command_for_key(key, input_key::InputCapabilities::SINGLE_LINE)
            {
                dialog.input.apply(command);
            }
        }
        _ => {}
    }

    EventResult::Continue
}

/// Inserts pasted text into the key field as one line while editing.
pub(crate) fn handle_paste(app: &mut App, pasted_text: &str) {
    if let AppMode::ApiKeyInput { dialog } = &mut app.mode
        && dialog.is_editing()
    {
        dialog.input.apply(InputCommand::InsertText(
            input_key::normalize_single_line_pasted_text(pasted_text),
        ));
    }
}

#[cfg(test)]
#[path = "api_key_test.rs"]
mod tests;
