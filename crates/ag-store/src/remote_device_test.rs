//! Paired remote-control device repository tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::{Database, DbError, RemoteDeviceRow};

/// Opens an in-memory database whose timestamps advance by one second per
/// write, starting at `100`.
async fn database_with_ticking_clock() -> Database {
    let clock = Arc::new(AtomicI64::new(100));

    Database::in_memory_with_timestamp_source(Arc::new(move || {
        clock.fetch_add(1, Ordering::SeqCst)
    }))
    .await
    .expect("db should open")
}

fn device_row(id: &str, name: &str, created_at: i64, last_seen_at: Option<i64>) -> RemoteDeviceRow {
    RemoteDeviceRow {
        created_at,
        id: id.to_string(),
        last_seen_at,
        name: name.to_string(),
    }
}

#[tokio::test]
async fn inserted_devices_list_in_pairing_order_without_a_last_seen_time() {
    // Arrange
    let database = database_with_ticking_clock().await;
    let devices = database.remote_devices();

    // Act
    devices
        .insert_device("device-b", "Phone", "hash-b")
        .await
        .expect("first device should insert");
    devices
        .insert_device("device-a", "Tablet", "hash-a")
        .await
        .expect("second device should insert");
    let listed = devices.list_devices().await.expect("devices should list");

    // Assert
    assert_eq!(
        listed,
        [
            device_row("device-b", "Phone", 100, None),
            device_row("device-a", "Tablet", 101, None),
        ]
    );
}

#[tokio::test]
async fn devices_paired_in_the_same_second_list_by_identifier() {
    // Arrange
    let database = Database::in_memory_with_timestamp_source(Arc::new(|| 50))
        .await
        .expect("db should open");
    let devices = database.remote_devices();
    devices
        .insert_device("device-b", "Phone", "hash-b")
        .await
        .expect("first device should insert");
    devices
        .insert_device("device-a", "Tablet", "hash-a")
        .await
        .expect("second device should insert");

    // Act
    let listed = devices.list_devices().await.expect("devices should list");

    // Assert
    assert_eq!(
        listed,
        [
            device_row("device-a", "Tablet", 50, None),
            device_row("device-b", "Phone", 50, None),
        ]
    );
}

#[tokio::test]
async fn token_hash_lookup_finds_only_the_matching_device() {
    // Arrange
    let database = database_with_ticking_clock().await;
    let devices = database.remote_devices();
    devices
        .insert_device("device-1", "Phone", "hash-1")
        .await
        .expect("device should insert");

    // Act
    let found = devices
        .find_device_by_token_hash("hash-1")
        .await
        .expect("lookup should succeed");
    let missing = devices
        .find_device_by_token_hash("hash-unknown")
        .await
        .expect("lookup should succeed");

    // Assert
    assert_eq!(found, Some(device_row("device-1", "Phone", 100, None)));
    assert_eq!(missing, None);
}

#[tokio::test]
async fn touch_records_the_connection_time_and_ignores_unknown_devices() {
    // Arrange
    let database = database_with_ticking_clock().await;
    let devices = database.remote_devices();
    devices
        .insert_device("device-1", "Phone", "hash-1")
        .await
        .expect("device should insert");

    // Act
    devices
        .touch_device("device-1")
        .await
        .expect("touch should succeed");
    devices
        .touch_device("device-unknown")
        .await
        .expect("unknown touch should be a no-op");
    let listed = devices.list_devices().await.expect("devices should list");

    // Assert
    assert_eq!(listed, [device_row("device-1", "Phone", 100, Some(101))]);
}

#[tokio::test]
async fn delete_removes_the_device_and_ignores_unknown_devices() {
    // Arrange
    let database = database_with_ticking_clock().await;
    let devices = database.remote_devices();
    devices
        .insert_device("device-1", "Phone", "hash-1")
        .await
        .expect("first device should insert");
    devices
        .insert_device("device-2", "Tablet", "hash-2")
        .await
        .expect("second device should insert");

    // Act
    devices
        .delete_device("device-1")
        .await
        .expect("delete should succeed");
    devices
        .delete_device("device-unknown")
        .await
        .expect("unknown delete should be a no-op");
    let listed = devices.list_devices().await.expect("devices should list");
    let revoked = devices
        .find_device_by_token_hash("hash-1")
        .await
        .expect("lookup should succeed");

    // Assert
    assert_eq!(listed, [device_row("device-2", "Tablet", 101, None)]);
    assert_eq!(revoked, None);
}

#[tokio::test]
async fn insert_rejects_a_reused_token_hash_or_identifier() {
    // Arrange
    let database = database_with_ticking_clock().await;
    let devices = database.remote_devices();
    devices
        .insert_device("device-1", "Phone", "hash-1")
        .await
        .expect("device should insert");

    // Act
    let reused_hash = devices.insert_device("device-2", "Tablet", "hash-1").await;
    let reused_id = devices.insert_device("device-1", "Tablet", "hash-2").await;
    let listed = devices.list_devices().await.expect("devices should list");

    // Assert
    assert!(matches!(reused_hash, Err(DbError::Query(_))));
    assert!(matches!(reused_id, Err(DbError::Query(_))));
    assert_eq!(listed, [device_row("device-1", "Phone", 100, None)]);
}
