//! Tests for the remote-control frame projection.

use crate::presentation::remote_control::{
    RemoteControlNotice, RemoteControlView, RemoteDeviceView, RemoteListenerView,
    RemotePairingRequestView, RemoteRelayView,
};

/// Builds a view with one connected device, one idle device, and one
/// pending pairing.
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
        notice: None,
        pairing_requests: vec![RemotePairingRequestView {
            confirmation: "K7QX".to_string(),
            device_name: "Laptop".to_string(),
            id: 1,
        }],
    }
}

#[test]
fn relay_view_encodes_the_link_as_square_half_block_rows() {
    // Arrange
    let link = "https://relay.example/#code=ABCD".to_string();

    // Act
    let relay = RemoteRelayView::new(link.clone(), true);

    // Assert
    let row_widths: Vec<usize> = relay
        .qr_code
        .iter()
        .map(|row| row.chars().count())
        .collect();
    let size = row_widths[0];
    assert_eq!(relay.link, link);
    assert!(relay.is_connected);
    assert!(row_widths.iter().all(|width| *width == size));
    assert_eq!(relay.qr_code.len(), size.div_ceil(2));
    assert_eq!(relay.qr_code[0].trim(), "");
    assert!(
        relay
            .qr_code
            .iter()
            .flat_map(|row| row.chars())
            .all(|cell| matches!(cell, '█' | '▀' | '▄' | ' '))
    );
}

#[test]
fn relay_view_has_no_qr_code_for_a_link_too_long_to_encode() {
    // Arrange
    let link = "x".repeat(8_000);

    // Act
    let relay = RemoteRelayView::new(link, false);

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

#[test]
fn status_bar_notice_appears_only_while_listening() {
    // Arrange
    let stopped = view_with_devices(None);
    let listening = view_with_devices(Some(RemoteListenerView {
        pairing_code: "ABCD-EFGH".to_string(),
        relay: None,
        url: "http://127.0.0.1:4646/".to_string(),
    }));

    // Act
    let stopped_notice = stopped.status_bar_notice();
    let listening_notice = listening.status_bar_notice();

    // Assert
    assert_eq!(stopped_notice, None);
    assert_eq!(
        listening_notice,
        Some(RemoteControlNotice {
            connected_devices: 1,
            pairing_requests: 1,
        })
    );
}
