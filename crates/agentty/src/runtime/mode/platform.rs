//! Key handling for the Platform tab's own actions.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::presentation::platform::PlatformState;
use crate::runtime::mode::remote_control;

/// Handles the Platform tab keys that list mode does not share with the
/// other tabs. While a sign-in waits, `o` and `c` open and copy its page.
/// Once signed in, `l` signs out, and a registered Agentty takes the Remote
/// tab's pairing, link, and device keys for remote control through the
/// platform's relay.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) {
    match &app.platform.view().state {
        PlatformState::SigningIn(_) => match key.code {
            KeyCode::Char('o') => app.open_platform_link(),
            KeyCode::Char('c') => app.copy_platform_link(),
            _ => {}
        },
        PlatformState::SignedIn(_) if key.code == KeyCode::Char('l') => {
            app.sign_out_platform().await;
        }
        PlatformState::SignedIn(_) if app.platform.view().is_registered() => {
            remote_control::handle(app, key).await;
        }
        PlatformState::SignedOut { .. }
        | PlatformState::SigningOut { .. }
        | PlatformState::Requesting
        | PlatformState::SignedIn(_) => {}
    }
}

#[cfg(test)]
#[path = "platform_test.rs"]
mod tests;
