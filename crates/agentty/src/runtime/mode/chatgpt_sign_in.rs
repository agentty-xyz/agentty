use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::presentation::chatgpt_sign_in::ChatGptSignInPhase;
use crate::runtime::EventResult;

/// Handles key input while the `ChatGPT` sign-in popup is visible.
///
/// `Enter` starts the sign-in from the confirmation step and dismisses a
/// finished one; `Esc` cancels at any step, including while the browser
/// sign-in runs.
pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::ChatGptSignIn { dialog } = &app.mode else {
        return EventResult::Continue;
    };

    match (&dialog.phase, key.code) {
        (ChatGptSignInPhase::Confirm, KeyCode::Enter) => app.start_chatgpt_sign_in(),
        (ChatGptSignInPhase::Finished { .. }, KeyCode::Enter) | (_, KeyCode::Esc) => {
            app.close_chatgpt_sign_in();
        }
        _ => {}
    }

    EventResult::Continue
}

#[cfg(test)]
#[path = "chatgpt_sign_in_test.rs"]
mod tests;
