use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, header};

use super::{
    ConnectionOrigin, MAX_UNAUTHENTICATED_SOCKETS, PairingRequest, RemoteConnection,
    RemoteServerStatus, host_is_allowed, origin_matches_host, replace_status,
};
use crate::device::{DeviceStore, DeviceStoreError};
use crate::server::{RemoteError, RemoteHost, RemoteServerConfig, start_with_entropy};
use crate::test_support::{
    FailingEntropy, MemoryDeviceStore, RecordingBackend, ReplayEntropy, StateFixture,
};

/// Server state over idle doubles.
fn state_fixture() -> StateFixture {
    StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::default(),
        Arc::new(ReplayEntropy::new(vec![0])),
    )
}

/// Builds headers carrying only `Host: host`.
fn host_header(host: &'static str) -> HeaderMap {
    headers(&[(header::HOST, host)])
}

#[test]
fn host_check_accepts_loopback_names_with_or_without_ports() {
    // Arrange
    let hosts = [
        "localhost",
        "localhost:4646",
        "127.0.0.1:4646",
        "[::1]:4646",
        "[::1]",
        "LOCALHOST:4646",
    ];

    // Act
    let results = hosts.map(|host| host_is_allowed(&host_header(host), &[]));

    // Assert
    assert_eq!(results, [true; 6]);
}

#[test]
fn host_check_accepts_configured_hosts_ignoring_case_and_port() {
    // Arrange
    let allowed = vec!["box.tailnet.ts.net".to_string()];

    // Act
    let tunneled = host_is_allowed(&host_header("Box.Tailnet.TS.net"), &allowed);
    let tunneled_with_port = host_is_allowed(&host_header("box.tailnet.ts.net:443"), &allowed);

    // Assert
    assert!(tunneled);
    assert!(tunneled_with_port);
}

#[test]
fn host_check_refuses_other_hosts_and_missing_host() {
    // Arrange
    let allowed = vec!["box.tailnet.ts.net".to_string()];
    let rebinding = host_header("evil.example:4646");
    let lookalike = host_header("box.tailnet.ts.net.evil.example");
    let bracketed_other = host_header("[2001:db8::1]:4646");
    let unclosed_bracket = host_header("[::2");

    // Act
    let results = [
        host_is_allowed(&rebinding, &allowed),
        host_is_allowed(&lookalike, &allowed),
        host_is_allowed(&bracketed_other, &allowed),
        host_is_allowed(&unclosed_bracket, &allowed),
        host_is_allowed(&HeaderMap::new(), &allowed),
    ];

    // Assert
    assert_eq!(results, [false; 5]);
}

#[test]
fn handshake_slots_stop_at_the_cap_per_origin_and_free_on_drop() {
    // Arrange
    let fixture = state_fixture();
    let mut slots: Vec<_> = (0..MAX_UNAUTHENTICATED_SOCKETS)
        .map(|_| fixture.state.reserve_handshake(ConnectionOrigin::Relay))
        .collect();

    // Act
    let over_cap = fixture.state.reserve_handshake(ConnectionOrigin::Relay);
    let local = fixture.state.reserve_handshake(ConnectionOrigin::Local);
    slots.pop();
    let after_release = fixture.state.reserve_handshake(ConnectionOrigin::Relay);

    // Assert
    assert!(slots.iter().all(Option::is_some));
    assert!(over_cap.is_none());
    assert!(local.is_some());
    assert!(after_release.is_some());
}

#[tokio::test]
async fn pairing_requests_are_listed_until_settled_or_withdrawn() {
    // Arrange
    let fixture = state_fixture();
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
fn settling_a_request_whose_socket_left_reports_no_delivery() {
    // Arrange
    let fixture = state_fixture();
    let (decision_rx, _guard) = fixture
        .state
        .request_pairing("Phone".to_string(), "ABCD".to_string());
    drop(decision_rx);

    // Act
    let delivered = fixture.state.settle_pairing(0, true);

    // Assert
    assert!(!delivered);
}

/// Builds headers from `(name, value)` pairs.
fn headers(pairs: &[(header::HeaderName, &'static str)]) -> HeaderMap {
    pairs
        .iter()
        .map(|(name, value)| (name.clone(), HeaderValue::from_static(value)))
        .collect()
}

#[test]
fn origin_check_allows_clients_without_origin() {
    // Arrange
    let headers = headers(&[(header::HOST, "127.0.0.1:4646")]);

    // Act
    let is_allowed = origin_matches_host(&headers);

    // Assert
    assert!(is_allowed);
}

#[test]
fn origin_check_accepts_same_host_over_http_and_https() {
    // Arrange
    let local = headers(&[
        (header::HOST, "127.0.0.1:4646"),
        (header::ORIGIN, "http://127.0.0.1:4646"),
    ]);
    let tunneled = headers(&[
        (header::HOST, "Box.Tailnet.ts.net"),
        (header::ORIGIN, "https://box.tailnet.ts.net"),
    ]);

    // Act
    let local_allowed = origin_matches_host(&local);
    let tunneled_allowed = origin_matches_host(&tunneled);

    // Assert
    assert!(local_allowed);
    assert!(tunneled_allowed);
}

#[test]
fn origin_check_rejects_other_sites_and_malformed_origins() {
    // Arrange
    let other_site = headers(&[
        (header::HOST, "127.0.0.1:4646"),
        (header::ORIGIN, "https://evil.example"),
    ]);
    let other_port = headers(&[
        (header::HOST, "127.0.0.1:4646"),
        (header::ORIGIN, "http://127.0.0.1:8080"),
    ]);
    let opaque = headers(&[(header::HOST, "127.0.0.1:4646"), (header::ORIGIN, "null")]);
    let missing_host = headers(&[(header::ORIGIN, "http://127.0.0.1:4646")]);

    // Act
    let results = [
        origin_matches_host(&other_site),
        origin_matches_host(&other_port),
        origin_matches_host(&opaque),
        origin_matches_host(&missing_host),
    ];

    // Assert
    assert_eq!(results, [false; 4]);
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
        relay: None,
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
        RemoteError::Bind(std::io::Error::other("in use")),
        RemoteError::DeviceStore(DeviceStoreError("locked".to_string())),
        RemoteError::Entropy,
    ];

    // Act
    let messages = errors.map(|error| error.to_string());

    // Assert
    assert_eq!(
        messages,
        [
            "Cannot listen for remote control: in use",
            "Remote device store failed: locked",
            "Cannot generate a secure pairing code",
        ]
    );
}

#[tokio::test]
async fn issue_pairing_code_reports_missing_entropy() {
    // Arrange
    let (_revisions_tx, revisions) = tokio::sync::watch::channel(0);
    let host = RemoteHost {
        devices: Arc::new(MemoryDeviceStore::default()) as Arc<dyn DeviceStore>,
        revisions,
        sessions: ag_session::SessionService::new(Arc::new(RecordingBackend::default())),
    };
    let config = RemoteServerConfig {
        allowed_hosts: Vec::new(),
        bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        relay: None,
    };
    let handle = start_with_entropy(config, host, Arc::new(FailingEntropy))
        .await
        .expect("loopback listener should bind");

    // Act
    let code = handle.issue_pairing_code();

    // Assert
    assert!(matches!(code, Err(RemoteError::Entropy)));
    handle.shutdown().await;
}
