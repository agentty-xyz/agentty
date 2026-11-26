//! Tests for the remote-control frame projection.

use ag_remote::RelayStatus;

use crate::presentation::remote_control::{
    LIGHT_CELL, RemoteControlView, RemoteDeviceView, RemotePairingRequestView, RemoteRelayView,
};

/// Builds a view with one connected device, one idle device, and one
/// pending pairing.
fn view_with_devices(relay: Option<RemoteRelayView>) -> RemoteControlView {
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
        notice: None,
        pairing_requests: vec![RemotePairingRequestView {
            confirmation: "K7QX".to_string(),
            device_name: "Laptop".to_string(),
            id: 1,
        }],
        relay,
        selected_device_index: 0,
    }
}

#[test]
fn relay_view_encodes_the_link_as_square_half_block_rows() {
    // Arrange
    let link = "https://relay.example/#code=ABCD".to_string();

    // Act
    let relay = RemoteRelayView::new(link.clone(), RelayStatus::Connected);

    // Assert
    let row_widths: Vec<usize> = relay
        .qr_code
        .iter()
        .map(|row| row.chars().count())
        .collect();
    let size = row_widths[0];
    assert_eq!(relay.link, link);
    assert_eq!(relay.status, RelayStatus::Connected);
    assert!(row_widths.iter().all(|width| *width == size));
    assert_eq!(relay.qr_code.len(), size.div_ceil(2));
    assert!(relay.qr_code[0].chars().all(|cell| cell == LIGHT_CELL));
    assert!(
        relay
            .qr_code
            .iter()
            .flat_map(|row| row.chars())
            .all(|cell| matches!(cell, '█' | '▀' | '▄' | LIGHT_CELL))
    );
}

#[test]
fn relay_view_has_no_qr_code_for_a_link_too_long_to_encode() {
    // Arrange
    let link = "x".repeat(8_000);

    // Act
    let relay = RemoteRelayView::new(link, RelayStatus::Connecting);

    // Assert
    assert_eq!(relay.qr_code, Vec::<String>::new());
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
