use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, header};

use super::{RemoteConnection, RemoteServerStatus, origin_matches_host, replace_connections};
use crate::device::{DeviceStore, DeviceStoreError};
use crate::server::{RemoteError, RemoteHost, RemoteServerConfig, start_with_entropy};
use crate::test_support::{FailingEntropy, MemoryDeviceStore, RecordingBackend};

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
fn replace_connections_reports_only_real_changes() {
    // Arrange
    let connection = RemoteConnection {
        device_id: "phone".to_string(),
        device_name: "Phone".to_string(),
    };
    let mut status = RemoteServerStatus::default();

    // Act
    let first = replace_connections(&mut status, vec![connection.clone()]);
    let repeated = replace_connections(&mut status, vec![connection.clone()]);

    // Assert
    assert!(first);
    assert!(!repeated);
    assert_eq!(status.connections, [connection]);
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
        bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
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
