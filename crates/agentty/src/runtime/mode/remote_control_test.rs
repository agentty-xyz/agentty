//! Remote-control popup key handling tests.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::handle;
use crate::app::App;
use crate::domain::session::SessionId;
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode};
use crate::runtime::EventResult;

/// Returns the restore target the popup reopens.
fn restore_view() -> ConfirmationViewMode {
    ConfirmationViewMode {
        scroll_offset: Some(3),
        session_id: SessionId::from("session-1"),
    }
}

/// Opens the popup with `selected_device_index` highlighted.
fn open_popup(app: &mut App, selected_device_index: usize) {
    app.mode = AppMode::RemoteControl {
        restore_view: restore_view(),
        selected_device_index,
    };
}

/// Sends one plain key press to the popup.
async fn press(app: &mut App, code: KeyCode) -> EventResult {
    handle(app, KeyEvent::new(code, KeyModifiers::NONE)).await
}

/// Returns the highlighted device row while the popup is open.
fn selected_device_index(app: &App) -> Option<usize> {
    match app.mode {
        AppMode::RemoteControl {
            selected_device_index,
            ..
        } => Some(selected_device_index),
        _ => None,
    }
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
async fn esc_restores_the_session_view() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    open_popup(&mut app, 0);

    // Act
    let event_result = press(&mut app, KeyCode::Esc).await;

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        &app.mode,
        AppMode::View {
            scroll_offset: Some(3),
            session_id,
        } if session_id.as_str() == "session-1"
    ));
}

#[tokio::test]
async fn keys_are_ignored_outside_the_popup() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;

    // Act
    let event_result = press(&mut app, KeyCode::Char('s')).await;

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::List));
    assert_eq!(app.remote_control.view().listener, None);
}

#[tokio::test]
async fn s_starts_and_stops_and_n_replaces_the_code() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.enable_remote_control(0, Vec::new(), None);
    open_popup(&mut app, 0);

    // Act
    press(&mut app, KeyCode::Char('s')).await;
    let started = app.remote_control.view().listener.clone();
    press(&mut app, KeyCode::Char('n')).await;
    let replaced = app.remote_control.view().listener.clone();
    press(&mut app, KeyCode::Char('s')).await;

    // Assert
    let started = started.expect("s should start the listener");
    let replaced = replaced.expect("n should keep the listener running");
    assert!(started.url.starts_with("http://127.0.0.1:"));
    assert_eq!(replaced.url, started.url);
    assert_ne!(replaced.pairing_code, started.pairing_code);
    assert_eq!(app.remote_control.view().listener, None);
    assert_eq!(selected_device_index(&app), Some(0));
}

#[tokio::test]
async fn o_and_c_keep_the_popup_open_without_a_relay_link() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.enable_remote_control(0, Vec::new(), None);
    open_popup(&mut app, 0);
    press(&mut app, KeyCode::Char('s')).await;

    // Act
    press(&mut app, KeyCode::Char('o')).await;
    press(&mut app, KeyCode::Char('c')).await;

    // Assert
    assert_eq!(app.remote_control.view().notice, None);
    assert_eq!(selected_device_index(&app), Some(0));
    app.stop_remote_control().await;
}

#[tokio::test]
async fn y_and_x_keep_the_popup_open_without_pending_pairings() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    open_popup(&mut app, 0);

    // Act
    press(&mut app, KeyCode::Char('y')).await;
    press(&mut app, KeyCode::Char('x')).await;

    // Assert
    assert_eq!(selected_device_index(&app), Some(0));
    assert_eq!(app.remote_control.view().pairing_requests, []);
}

#[tokio::test]
async fn j_and_k_move_within_the_device_list() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    pair_devices(&app, &["phone", "tablet"]).await;
    app.revoke_remote_device("missing").await;
    open_popup(&mut app, 0);

    // Act
    press(&mut app, KeyCode::Char('j')).await;
    press(&mut app, KeyCode::Down).await;
    let after_down = selected_device_index(&app);
    press(&mut app, KeyCode::Char('k')).await;
    press(&mut app, KeyCode::Up).await;
    press(&mut app, KeyCode::Char('z')).await;

    // Assert
    assert_eq!(after_down, Some(1));
    assert_eq!(selected_device_index(&app), Some(0));
}

#[tokio::test]
async fn d_removes_the_selected_device() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    pair_devices(&app, &["phone", "tablet"]).await;
    app.revoke_remote_device("missing").await;
    open_popup(&mut app, 1);

    // Act
    press(&mut app, KeyCode::Char('d')).await;
    press(&mut app, KeyCode::Char('d')).await;
    press(&mut app, KeyCode::Char('d')).await;

    // Assert
    assert_eq!(app.remote_control.view().devices, []);
    assert_eq!(selected_device_index(&app), Some(0));
}
