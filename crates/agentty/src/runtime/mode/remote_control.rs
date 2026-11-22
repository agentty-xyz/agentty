//! Key handling for the Remote tab's own actions.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;

/// Handles the Remote tab keys that list mode does not share with the other
/// tabs: pairing codes, relay links, pairing approval, and device removal.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('n') => app.issue_remote_pairing_code(),
        KeyCode::Char('o') => app.open_remote_link(),
        KeyCode::Char('c') => app.copy_remote_link(),
        KeyCode::Char('y') => app.settle_remote_pairing(true),
        KeyCode::Char('x') => app.settle_remote_pairing(false),
        KeyCode::Char('d') => {
            let view = app.remote_control.view();
            let device_id = view
                .devices
                .get(view.selected_device_index)
                .map(|device| device.id.clone());
            if let Some(device_id) = device_id {
                app.revoke_remote_device(&device_id).await;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "remote_control_test.rs"]
mod tests;
