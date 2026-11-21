//! Key handling for the `/remote-control` popup.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;

/// Handles key input while the remote-control popup is visible.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::RemoteControl {
        restore_view,
        selected_device_index,
    } = &app.mode
    else {
        return EventResult::Continue;
    };
    let restore_view = restore_view.clone();
    let mut selected_device_index = *selected_device_index;

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = restore_view.into_view_mode();

            return EventResult::Continue;
        }
        KeyCode::Char('n') => app.issue_remote_pairing_code(),
        KeyCode::Char('s') => {
            if app.remote_control.view().listener.is_some() {
                app.stop_remote_control().await;
            } else {
                app.open_remote_control().await;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            selected_device_index = selected_device_index.saturating_add(1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            selected_device_index = selected_device_index.saturating_sub(1);
        }
        KeyCode::Char('d') => {
            let device_id = app
                .remote_control
                .view()
                .devices
                .get(selected_device_index)
                .map(|device| device.id.clone());
            if let Some(device_id) = device_id {
                app.revoke_remote_device(&device_id).await;
            }
        }
        _ => {}
    }

    let last_device_index = app.remote_control.view().devices.len().saturating_sub(1);
    app.mode = AppMode::RemoteControl {
        restore_view,
        selected_device_index: selected_device_index.min(last_device_index),
    };

    EventResult::Continue
}

#[cfg(test)]
#[path = "remote_control_test.rs"]
mod tests;
