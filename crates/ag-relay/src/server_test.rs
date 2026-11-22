use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::{Semaphore, watch};

use super::RelayState;
use crate::RelayLimits;
use crate::registry::Registry;

/// Peer address of the connection in these tests.
const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 4000);

/// State that reads the client address from `header`, when given.
fn state(header: Option<&'static str>) -> RelayState {
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    RelayState {
        client_address_header: header.map(HeaderName::from_static),
        handshakes: Arc::new(Semaphore::new(1)),
        limits: RelayLimits::default(),
        open_sockets_tx: watch::Sender::new(0),
        registry: Registry::new(),
        shutdown_rx,
    }
}

fn headers(value: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-real-ip", HeaderValue::from_static(value));

    headers
}

#[test]
fn client_address_comes_from_the_configured_header() {
    // Arrange
    let state = state(Some("x-real-ip"));

    // Act
    let address = state.client_address(PEER, &headers(" 203.0.113.9 "));

    // Assert
    assert_eq!(address, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)));
}

#[test]
fn client_address_falls_back_to_the_peer() {
    // Arrange
    let with_header = state(Some("x-real-ip"));
    let without_header = state(None);

    // Act
    let missing = with_header.client_address(PEER, &HeaderMap::new());
    let not_one_address = with_header.client_address(PEER, &headers("203.0.113.9, 10.0.0.1"));
    let unconfigured = without_header.client_address(PEER, &headers("203.0.113.9"));

    // Assert
    assert_eq!(missing, PEER.ip());
    assert_eq!(not_one_address, PEER.ip());
    assert_eq!(unconfigured, PEER.ip());
}

#[test]
fn handshake_reservations_stop_at_the_limit() {
    // Arrange
    let state = state(None);

    // Act
    let first = state.reserve_handshake();
    let second = state.reserve_handshake();
    drop(first);
    let after_release = state.reserve_handshake();

    // Assert
    assert!(second.is_none());
    assert!(after_release.is_some());
}
