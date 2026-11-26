use std::sync::Arc;
use std::thread;

use ag_noise::StaticKeypair;
use tokio::sync::oneshot;

use super::{
    MAX_UNAUTHENTICATED_SOCKETS, PairingRequest, RegisteredConnection, RemoteConnection,
    RemoteServerStatus, replace_status,
};
use crate::device::{DeviceStore, DeviceStoreError};
use crate::relay::{RelayStatus, RemoteRelayConfig};
use crate::server::{RemoteError, RemoteHost, RemoteServerConfig, start_with_entropy};
use crate::source_support::{
    FailingEntropy, MemoryDeviceStore, RecordingBackend, StateFixture, loopback_listener,
};

#[test]
fn handshake_slots_stop_at_the_cap_and_free_on_drop() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let mut slots: Vec<_> = (0..MAX_UNAUTHENTICATED_SOCKETS)
        .map(|_| fixture.state.reserve_handshake())
        .collect();

    // Act
    let over_cap = fixture.state.reserve_handshake();
    slots.pop();
    let after_release = fixture.state.reserve_handshake();

    // Assert
    assert!(slots.iter().all(Option::is_some));
    assert!(over_cap.is_none());
    assert!(after_release.is_some());
}

#[tokio::test]
async fn pairing_requests_are_listed_until_settled_or_withdrawn() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let status = fixture.state.status_tx.subscribe();

    // Act
    let (approved_rx, _approved_guard) = fixture
        .state
        .request_pairing("Phone".to_string(), "ABCD".to_string());
    let (_withdrawn_rx, withdrawn_guard) = fixture
        .state
        .request_pairing("Tablet".to_string(), "EFGH".to_string());
    let listed = status.borrow().pairing_requests.clone();
    let approved = fixture.state.settle_pairing(0, true);
    let settled_twice = fixture.state.settle_pairing(0, false);
    drop(withdrawn_guard);
    let unknown = fixture.state.settle_pairing(1, true);

    // Assert
    assert_eq!(
        listed,
        [
            PairingRequest {
                confirmation: "ABCD".to_string(),
                device_name: "Phone".to_string(),
                id: 0,
            },
            PairingRequest {
                confirmation: "EFGH".to_string(),
                device_name: "Tablet".to_string(),
                id: 1,
            },
        ]
    );
    assert!(approved);
    assert_eq!(approved_rx.await, Ok(true));
    assert!(!settled_twice);
    assert!(!unknown);
    assert_eq!(status.borrow().pairing_requests, []);
}

#[test]
fn a_publication_that_waits_reads_the_registry_as_it_is_when_it_sends() {
    // Arrange: the only device reloads. Its old socket leaves and starts
    // publishing while a reader holds the status, so the publication waits.
    let device = |device_id: &str| RemoteConnection {
        device_id: device_id.to_string(),
        device_name: "Phone".to_string(),
    };
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let status = fixture.state.status_tx.subscribe();
    let (_old_close_rx, old_guard) = fixture.state.register(device("old"));
    let held_status = status.borrow();
    let leaving = thread::spawn(move || drop(old_guard));
    while !fixture.state.registry().entries.is_empty() {
        thread::yield_now();
    }

    // Act: the new socket registers while the old one's publication still
    // waits, and the reader then lets it send.
    let (new_close_tx, _new_close_rx) = oneshot::channel();
    fixture.state.registry().entries.insert(
        1,
        RegisteredConnection {
            close_tx: new_close_tx,
            connection: device("new"),
        },
    );
    drop(held_status);
    leaving.join().expect("leaving socket should finish");

    // Assert: a status read before the wait would have listed nobody.
    assert_eq!(status.borrow().connections, [device("new")]);
}

#[test]
fn current_status_lists_connections_pairing_requests_and_the_relay_link() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let connection = RemoteConnection {
        device_id: "phone".to_string(),
        device_name: "Phone".to_string(),
    };
    let starting = fixture.state.current_status().relay;
    let (_close_rx, _registration) = fixture.state.register(connection.clone());
    let (_decision_rx, _pairing) = fixture
        .state
        .request_pairing("Tablet".to_string(), "ABCD".to_string());
    fixture.state.set_relay_status(RelayStatus::Connected);

    // Act
    let status = fixture.state.current_status();

    // Assert
    assert_eq!(starting, RelayStatus::Connecting);
    assert_eq!(
        status,
        RemoteServerStatus {
            connections: vec![connection],
            pairing_requests: vec![PairingRequest {
                confirmation: "ABCD".to_string(),
                device_name: "Tablet".to_string(),
                id: 0,
            }],
            relay: RelayStatus::Connected,
        }
    );
    assert_eq!(*fixture.state.status_tx.borrow(), status);
}

#[test]
fn settling_a_request_whose_socket_left_reports_no_delivery() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let (decision_rx, _guard) = fixture
        .state
        .request_pairing("Phone".to_string(), "ABCD".to_string());
    drop(decision_rx);

    // Act
    let delivered = fixture.state.settle_pairing(0, true);

    // Assert
    assert!(!delivered);
}

#[test]
fn replace_status_reports_only_real_changes() {
    // Arrange
    let next = RemoteServerStatus {
        connections: vec![RemoteConnection {
            device_id: "phone".to_string(),
            device_name: "Phone".to_string(),
        }],
        pairing_requests: Vec::new(),
        relay: RelayStatus::Connecting,
    };
    let mut status = RemoteServerStatus::default();

    // Act
    let first = replace_status(&mut status, next.clone());
    let repeated = replace_status(&mut status, next.clone());

    // Assert
    assert!(first);
    assert!(!repeated);
    assert_eq!(status, next);
}

#[test]
fn remote_errors_describe_their_cause() {
    // Arrange
    let errors = [
        RemoteError::DeviceStore(DeviceStoreError("locked".to_string())),
        RemoteError::Entropy,
    ];

    // Act
    let messages = errors.map(|error| error.to_string());

    // Assert
    assert_eq!(
        messages,
        [
            "Remote device store failed: locked",
            "Cannot generate a secure pairing code",
        ]
    );
}

#[tokio::test]
async fn issue_pairing_code_reports_missing_entropy() {
    // Arrange: nothing listens on the relay address any more, so the link
    // only retries.
    let (listener, relay_address) = loopback_listener().await;
    drop(listener);
    let (_revisions_tx, revisions) = tokio::sync::watch::channel(0);
    let host = RemoteHost {
        devices: Arc::new(MemoryDeviceStore::default()) as Arc<dyn DeviceStore>,
        revisions,
        sessions: ag_session::SessionService::new(Arc::new(RecordingBackend::default())),
    };
    let config = RemoteServerConfig {
        relay: RemoteRelayConfig {
            instance_key: StaticKeypair::generate().expect("instance key"),
            url: format!("ws://{relay_address}"),
        },
    };
    let handle = start_with_entropy(config, host, Arc::new(FailingEntropy));

    // Act
    let code = handle.issue_pairing_code();

    // Assert
    assert!(matches!(code, Err(RemoteError::Entropy)));
    handle.shutdown().await;
}
