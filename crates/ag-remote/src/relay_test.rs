use std::io;
use std::net::{self, Ipv4Addr};
use std::time::Duration;

use ag_noise::PublicKey;
use tokio::io::AsyncReadExt;
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::http::Uri;

use super::{Dialer, INITIAL_RETRY_DELAY, REGISTER_PATH, RelayEndpoint, retry_delay_after};
use crate::relay::{pairing_link, pairing_link_on_page};
use crate::source_support::{
    LOOPBACK_GUARD, TLS_HANDSHAKE_RECORD, TlsFixture, loopback_listener, untrusting_tls_config,
};

#[test]
fn dialer_takes_the_host_the_explicit_or_default_port_and_the_scheme_security() {
    let cases = [
        (
            "ws://127.0.0.1:8787/v1/register",
            Some(("127.0.0.1", 8787, false)),
        ),
        (
            "ws://relay.example/v1/register",
            Some(("relay.example", 80, false)),
        ),
        (
            "wss://relay.example/v1/register",
            Some(("relay.example", 443, true)),
        ),
        (
            "wss://relay.example:8443/v1/register",
            Some(("relay.example", 8443, true)),
        ),
        ("ws://[::1]:8787/v1/register", Some(("::1", 8787, false))),
        (
            "wss://[2001:db8::1]/v1/register",
            Some(("2001:db8::1", 443, true)),
        ),
        ("http://relay.example:8080/v1/register", None),
        ("/v1/register", None),
    ];

    for (url, expected) in cases {
        // Arrange
        let uri: Uri = url.parse().expect("test URL should parse");

        // Act
        let dialed = Dialer::endpoint(&uri)
            .map(|endpoint: RelayEndpoint<'_>| (endpoint.host, endpoint.port, endpoint.is_secure));

        // Assert
        assert_eq!(dialed, expected, "{url}");
    }
}

#[test]
fn pairing_link_on_another_page_keeps_the_page_and_the_fragment_format() {
    // Arrange
    let key = PublicKey::from_bytes(&[0xAB; 32]).expect("key should load");

    // Act
    let link = pairing_link_on_page(
        "https://platform.example/sessions/",
        "wss://relay.platform.example/",
        &key,
        "ABCD-EFGH",
    );

    // Assert
    assert_eq!(
        link,
        format!(
            "https://platform.example/sessions/#relay=wss%3A%2F%2Frelay%2Eplatform%2Eexample&key={}&code=ABCD%2DEFGH",
            "ab".repeat(32)
        )
    );
}

#[test]
fn pairing_link_points_at_the_relay_page_and_keeps_secrets_in_the_fragment() {
    // Arrange
    let key = PublicKey::from_bytes(&[0xAB; 32]).expect("key should load");

    // Act
    let secure = pairing_link("wss://relay.example/", &key, "ABCD-EFGH");
    let plain = pairing_link("ws://127.0.0.1:8787", &key, "ABCD-EFGH");

    // Assert
    assert_eq!(
        secure,
        format!(
            "https://relay.example/#relay=wss%3A%2F%2Frelay%2Eexample&key={}&code=ABCD%2DEFGH",
            "ab".repeat(32)
        )
    );
    assert!(plain.starts_with("http://127.0.0.1:8787/#relay=ws%3A%2F%2F127%2E0%2E0%2E1%3A8787&"));
}

#[test]
fn retry_delay_resets_after_a_registered_link_and_keeps_growing_otherwise() {
    // Arrange
    let grown = Duration::from_secs(8);

    // Act
    let after_registered = retry_delay_after(grown, true);
    let after_failure = retry_delay_after(grown, false);

    // Assert
    assert_eq!(after_registered, INITIAL_RETRY_DELAY);
    assert_eq!(after_failure, grown);
}

#[tokio::test]
async fn dial_starts_its_scheme_handshake_and_fails_when_the_relay_closes_during_it() {
    // `ws` sends the `WebSocket` upgrade request, `wss` the TLS `ClientHello`.
    for (scheme, expected_first_byte) in [("ws", b'G'), ("wss", TLS_HANDSHAKE_RECORD)] {
        // Arrange
        let (listener, address) = loopback_listener().await;
        let dialer = Dialer {
            base_url: format!("{scheme}://{address}"),
            tls: Some(untrusting_tls_config()),
        };
        let dial = tokio::spawn(async move { dialer.open(REGISTER_PATH).await.is_some() });
        let (mut accepted, _) = timeout(LOOPBACK_GUARD, listener.accept())
            .await
            .expect("dial should reach the relay")
            .expect("dial should connect");
        let mut first_byte = [0; 1];
        accepted
            .read_exact(&mut first_byte)
            .await
            .expect("dial should send a first message");

        // Act
        drop(accepted);
        let is_open = timeout(LOOPBACK_GUARD, dial)
            .await
            .expect("dial should finish")
            .expect("dial task should not panic");

        // Assert
        assert_eq!(first_byte, [expected_first_byte], "{scheme}");
        assert!(!is_open, "{scheme}");
    }
}

#[tokio::test]
async fn secure_dial_completes_tls_and_the_upgrade_through_a_trusted_relay() {
    // Arrange
    let fixture = TlsFixture::new();
    let (listener, address) = loopback_listener().await;
    let acceptor = fixture.acceptor.clone();
    let relay = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.ok()?;
        let secured = acceptor.accept(connection).await.ok()?;

        accept_async(secured).await.ok()
    });
    let dialer = Dialer {
        base_url: format!("wss://{address}"),
        tls: Some(fixture.client),
    };

    // Act
    let stream = timeout(LOOPBACK_GUARD, dialer.open(REGISTER_PATH))
        .await
        .expect("dial should finish");
    let relay_socket = timeout(LOOPBACK_GUARD, relay)
        .await
        .expect("relay should finish")
        .expect("relay task should not panic");

    // Assert
    assert!(stream.is_some());
    assert!(relay_socket.is_some());
}

#[tokio::test]
async fn secure_dial_without_tls_settings_fails_without_connecting() {
    // Arrange
    let listener = net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listener should bind");
    listener
        .set_nonblocking(true)
        .expect("listener should stop blocking");
    let address = listener
        .local_addr()
        .expect("listener should have an address");
    let dialer = Dialer {
        base_url: format!("wss://{address}"),
        tls: None,
    };

    // Act
    let stream = timeout(LOOPBACK_GUARD, dialer.open(REGISTER_PATH))
        .await
        .expect("dial should finish");

    // Assert
    assert!(stream.is_none());
    assert_eq!(
        listener.accept().map_err(|error| error.kind()).err(),
        Some(io::ErrorKind::WouldBlock)
    );
}
