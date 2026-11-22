//! Remote tab key handling tests.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::handle;
use crate::app::App;

/// Sends one plain key press to the Remote tab's handler.
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

#[tokio::test]
async fn n_replaces_the_code_of_a_running_listener() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.enable_remote_control(0, Vec::new(), None).await;
    app.start_remote_control().await;
    let started = app.remote_control.view().listener.clone();

    // Act
    press(&mut app, KeyCode::Char('n')).await;

    // Assert
    let started = started.expect("the listener should start");
    let replaced = app
        .remote_control
        .view()
        .listener
        .clone()
        .expect("n should keep the listener running");
    assert_eq!(replaced.url, started.url);
    assert_ne!(replaced.pairing_code, started.pairing_code);
    app.stop_remote_control().await;
}

#[tokio::test]
async fn o_and_c_do_nothing_without_a_relay_link() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.enable_remote_control(0, Vec::new(), None).await;
    app.start_remote_control().await;

    // Act
    press(&mut app, KeyCode::Char('o')).await;
    press(&mut app, KeyCode::Char('c')).await;

    // Assert
    assert_eq!(app.remote_control.view().notice, None);
    app.stop_remote_control().await;
}

#[tokio::test]
async fn y_and_x_do_nothing_without_pending_pairings() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;

    // Act
    press(&mut app, KeyCode::Char('y')).await;
    press(&mut app, KeyCode::Char('x')).await;

    // Assert
    assert_eq!(app.remote_control.view().pairing_requests, []);
    assert_eq!(app.remote_control.view().error, None);
}

#[tokio::test]
async fn d_removes_the_selected_device_and_other_keys_do_nothing() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    pair_devices(&app, &["phone", "tablet"]).await;
    app.enable_remote_control(0, Vec::new(), None).await;
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
