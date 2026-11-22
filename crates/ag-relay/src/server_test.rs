use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::watch;

use super::{RelayState, limit_address};
use crate::RelayLimits;
use crate::admission::AdmitAll;
use crate::limit::HandshakeLimiter;
use crate::registry::Registry;

/// Peer address of the connection in these tests.
const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 4000);

/// State that reads the client address from `header`, when given.
fn state(header: Option<&'static str>) -> RelayState {
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let limits = RelayLimits::default();

    RelayState {
        admission: Arc::new(AdmitAll),
        client_address_header: header.map(HeaderName::from_static),
        handshakes: Arc::new(HandshakeLimiter::new(&limits)),
        limits,
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

fn address(text: &str) -> IpAddr {
    text.parse().expect("address should parse")
}

#[test]
fn client_address_comes_from_the_configured_header() {
    // Arrange
    let state = state(Some("x-real-ip"));

    // Act
    let from_header = state.client_address(PEER, &headers(" 203.0.113.9 "));
    let from_ipv6_header = state.client_address(PEER, &headers("2001:db8:1:2:3:4:5:6"));

    // Assert
    assert_eq!(from_header, address("203.0.113.9"));
    assert_eq!(from_ipv6_header, address("2001:db8:1:2::"));
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
fn limits_count_ipv6_clients_by_network_and_mapped_addresses_as_ipv4() {
    // Arrange
    let addresses = [
        "198.51.100.7",
        "::ffff:198.51.100.7",
        "2001:db8:1:2:aaaa::1",
        "2001:db8:1:2:bbbb::2",
        "2001:db8:1:3::1",
    ];

    // Act
    let counted = addresses.map(|text| limit_address(address(text)));

    // Assert
    assert_eq!(
        counted,
        [
            address("198.51.100.7"),
            address("198.51.100.7"),
            address("2001:db8:1:2::"),
            address("2001:db8:1:2::"),
            address("2001:db8:1:3::"),
        ]
    );
}
