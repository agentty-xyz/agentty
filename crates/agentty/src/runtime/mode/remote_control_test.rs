//! Remote-control key handling tests for a registered Platform tab.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::handle;
use crate::app::App;
use crate::infra::platform::RemoteControlService;

/// Sends one plain key press to the remote-control key handler.
async fn press(app: &mut App, code: KeyCode) {
    handle(app, KeyEvent::new(code, KeyModifiers::NONE)).await;
}

/// Stores paired devices directly in the app database.
async fn pair_devices(app: &App, ids: &[&str]) {
    for id in ids {
        app.services
            .db()
            .remote_devices()
            .insert_device(id, id, &format!("hash-{id}"))
            .await
            .expect("device should be stored");
    }
}

/// Returns the pairing link shown while remote control runs.
fn shown_link(app: &App) -> Option<String> {
    app.remote_control
        .view()
        .relay
        .as_ref()
        .map(|relay| relay.link.clone())
}

/// Hands the app a platform relay where nothing listens, so the real server
/// starts and keeps retrying without reaching anything.
async fn offer_unreachable_platform_relay(app: &mut App) {
    app.set_platform_relay(Some(RemoteControlService {
        pairing_url: "https://platform.example/sessions/".to_string(),
        relay_url: "ws://127.0.0.1:9".to_string(),
    }))
    .await;
}

#[tokio::test]
async fn n_replaces_the_pairing_link_of_running_remote_control() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    offer_unreachable_platform_relay(&mut app).await;
    app.start_remote_control().await;
    let started = shown_link(&app);

    // Act
    press(&mut app, KeyCode::Char('n')).await;

    // Assert
    let started = started.expect("remote control should start");
    let replaced = shown_link(&app).expect("n should keep remote control running");
    assert!(started.starts_with("https://platform.example/sessions/#relay="));
    assert_ne!(replaced, started);
    app.stop_remote_control().await;
}

#[tokio::test]
async fn link_and_pairing_keys_do_nothing_while_remote_control_is_off() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;

    // Act
    press(&mut app, KeyCode::Char('n')).await;
    press(&mut app, KeyCode::Char('o')).await;
    press(&mut app, KeyCode::Char('c')).await;
    press(&mut app, KeyCode::Char('y')).await;
    press(&mut app, KeyCode::Char('x')).await;

    // Assert
    assert_eq!(app.remote_control.view().relay, None);
    assert_eq!(app.remote_control.view().notice, None);
    assert_eq!(app.remote_control.view().pairing_requests, []);
    assert_eq!(app.remote_control.view().error, None);
}

#[tokio::test]
async fn d_removes_the_selected_device_and_other_keys_do_nothing() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    pair_devices(&app, &["phone", "tablet"]).await;
    app.refresh_remote_devices().await;
    app.remote_control.select_next_device();

    // Act
    press(&mut app, KeyCode::Char('z')).await;
    let untouched: Vec<String> = device_ids(&app);
    press(&mut app, KeyCode::Char('d')).await;
    let after_first_removal = device_ids(&app);
    press(&mut app, KeyCode::Char('d')).await;
    press(&mut app, KeyCode::Char('d')).await;

    // Assert
    assert_eq!(untouched, ["phone", "tablet"]);
    assert_eq!(after_first_removal, ["phone"]);
    assert_eq!(app.remote_control.view().devices, []);
    assert_eq!(app.remote_control.view().selected_device_index, 0);
}

/// Returns the identifiers of the paired devices the tab lists.
fn device_ids(app: &App) -> Vec<String> {
    app.remote_control
        .view()
        .devices
        .iter()
        .map(|device| device.id.clone())
        .collect()
}
