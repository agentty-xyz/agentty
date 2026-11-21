//! Tests for the remote-control frame projection.

use crate::presentation::remote_control::{
    RemoteControlView, RemoteDeviceView, RemoteListenerView,
};

/// Builds a view with one connected and one idle device.
fn view_with_devices(listener: Option<RemoteListenerView>) -> RemoteControlView {
    RemoteControlView {
        devices: vec![
            RemoteDeviceView {
                id: "phone".to_string(),
                is_connected: true,
                name: "Phone".to_string(),
            },
            RemoteDeviceView {
                id: "tablet".to_string(),
                is_connected: false,
                name: "Tablet".to_string(),
            },
        ],
        error: None,
        listener,
    }
}

#[test]
fn connected_device_count_counts_only_connected_devices() {
    // Arrange
    let view = view_with_devices(None);

    // Act
    let connected = view.connected_device_count();

    // Assert
    assert_eq!(connected, 1);
}

#[test]
fn status_bar_connections_appear_only_while_listening() {
    // Arrange
    let stopped = view_with_devices(None);
    let listening = view_with_devices(Some(RemoteListenerView {
        pairing_code: "ABCD-EFGH".to_string(),
        url: "http://127.0.0.1:4646/".to_string(),
    }));

    // Act
    let stopped_connections = stopped.status_bar_connections();
    let listening_connections = listening.status_bar_connections();

    // Assert
    assert_eq!(stopped_connections, None);
    assert_eq!(listening_connections, Some(1));
}
