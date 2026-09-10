use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::handle;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;

#[tokio::test]
async fn test_handle_esc_closes_sync_blocked_popup() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::SyncBlockedPopup {
        default_branch: None,
        is_loading: false,
        message: "Main is dirty".to_string(),
        project_name: None,
        title: "Sync blocked".to_string(),
    };

    // Act
    let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_enter_closes_sync_blocked_popup() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::SyncBlockedPopup {
        default_branch: None,
        is_loading: false,
        message: "Main is dirty".to_string(),
        project_name: None,
        title: "Sync blocked".to_string(),
    };

    // Act
    let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_other_key_keeps_sync_blocked_popup_open() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::SyncBlockedPopup {
        default_branch: None,
        is_loading: false,
        message: "Main is dirty".to_string(),
        project_name: None,
        title: "Sync blocked".to_string(),
    };

    // Act
    let event_result = handle(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::SyncBlockedPopup { .. }));
}

#[tokio::test]
async fn test_handle_enter_does_not_close_loading_sync_popup() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::SyncBlockedPopup {
        default_branch: None,
        is_loading: true,
        message: "Synchronizing...".to_string(),
        project_name: None,
        title: "Sync in progress".to_string(),
    };

    // Act
    let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::SyncBlockedPopup {
            is_loading: true,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_r_keeps_sync_blocked_popup_open() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::SyncBlockedPopup {
        default_branch: None,
        is_loading: false,
        message: "Sync failed".to_string(),
        project_name: None,
        title: "Sync failed".to_string(),
    };

    // Act
    let event_result = handle(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::SyncBlockedPopup { .. }));
}
