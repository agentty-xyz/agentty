use super::*;

#[test]
fn new_returns_registry_with_provider_name() {
    // Arrange / Act
    let registry = AppServerSessionRegistry::<String>::new("test-provider");

    // Assert
    assert_eq!(registry.provider_name(), "test-provider");
}

#[test]
fn store_and_take_session_round_trips_runtime() {
    // Arrange
    let registry = AppServerSessionRegistry::<String>::new("test");
    registry
        .store_session_or_recover("session-1".to_string(), "runtime-a".to_string())
        .expect("store should succeed");

    // Act
    let taken = registry
        .take_session("session-1")
        .expect("take should succeed");

    // Assert
    assert_eq!(taken, Some("runtime-a".to_string()));
}

#[test]
fn take_session_returns_none_for_missing_id() {
    // Arrange
    let registry = AppServerSessionRegistry::<String>::new("test");

    // Act
    let taken = registry
        .take_session("missing")
        .expect("take should succeed");

    // Assert
    assert_eq!(taken, None);
}

#[test]
fn take_session_removes_entry_from_registry() {
    // Arrange
    let registry = AppServerSessionRegistry::<String>::new("test");
    registry
        .store_session_or_recover("session-1".to_string(), "runtime-a".to_string())
        .expect("store should succeed");
    let _ = registry.take_session("session-1");

    // Act
    let second_take = registry
        .take_session("session-1")
        .expect("take should succeed");

    // Assert
    assert_eq!(second_take, None);
}

#[test]
fn store_session_or_recover_stores_runtime_on_healthy_lock() {
    // Arrange
    let registry = AppServerSessionRegistry::<String>::new("test");

    // Act
    let result =
        registry.store_session_or_recover("session-1".to_string(), "runtime-a".to_string());

    // Assert
    assert!(result.is_ok());
    let taken = registry
        .take_session("session-1")
        .expect("take should succeed");
    assert_eq!(taken, Some("runtime-a".to_string()));
}

#[test]
fn clone_shares_underlying_session_map() {
    // Arrange
    let registry = AppServerSessionRegistry::<String>::new("test");
    let cloned = registry.clone();
    registry
        .store_session_or_recover("session-1".to_string(), "runtime-a".to_string())
        .expect("store should succeed");

    // Act
    let taken = cloned
        .take_session("session-1")
        .expect("take on clone should succeed");

    // Assert
    assert_eq!(taken, Some("runtime-a".to_string()));
}

#[test]
fn cancel_active_turn_signals_registered_token() {
    // Arrange
    let registry = AppServerSessionRegistry::<String>::new("test");
    let active_turn = registry
        .register_active_turn("session-1")
        .expect("register should succeed");
    let token = active_turn.token();

    // Act
    let did_cancel = registry
        .cancel_active_turn("session-1")
        .expect("cancel should succeed");

    // Assert
    assert!(did_cancel);
    assert!(token.is_cancelled());
}

#[test]
fn active_turn_guard_unregisters_on_drop() {
    // Arrange
    let registry = AppServerSessionRegistry::<String>::new("test");
    let active_turn = registry
        .register_active_turn("session-1")
        .expect("register should succeed");

    // Act
    drop(active_turn);
    let did_cancel = registry
        .cancel_active_turn("session-1")
        .expect("cancel should succeed");

    // Assert
    assert!(!did_cancel);
}
