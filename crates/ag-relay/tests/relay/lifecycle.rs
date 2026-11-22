//! Starting, probing, and stopping the relay.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use ag_relay::{REGISTER_PATH, RelayConfig, RelayError, RelayLimits};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::support::{
    TestResult, connect_device, joined_stream, keypair, next_close, open, register, start,
};

#[tokio::test]
async fn health_endpoint_answers_ok() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let mut connection = TcpStream::connect(relay.local_addr()).await?;

    // Act
    connection
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: relay\r\nConnection: close\r\n\r\n")
        .await?;
    let mut response = String::new();
    connection.read_to_string(&mut response).await?;

    // Assert
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.ends_with("ok"));

    Ok(())
}

#[tokio::test]
async fn start_reports_unusable_configuration() -> TestResult {
    // Arrange
    let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let config = RelayConfig {
        bind_addr: occupied.local_addr()?,
        client_address_header: None,
        limits: RelayLimits::default(),
    };
    let bad_header = RelayConfig {
        bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        client_address_header: Some("not a header".to_string()),
        limits: RelayLimits::default(),
    };

    // Act
    let bind_error = ag_relay::start(config).await.err();
    let header_error = ag_relay::start(bad_header).await.err();

    // Assert
    assert!(matches!(bind_error, Some(RelayError::Bind(_))));
    assert!(matches!(header_error, Some(RelayError::InvalidHeaderName)));
    assert_eq!(
        header_error.map(|error| error.to_string()),
        Some("Client address header is not a valid header name".to_string())
    );

    Ok(())
}

#[tokio::test]
async fn shutdown_closes_every_kind_of_connection_and_releases_the_port() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        accept_timeout: Duration::from_mins(1),
        handshake_timeout: Duration::from_mins(1),
        ..RelayLimits::default()
    };
    let relay = start(limits).await?;
    let address = relay.local_addr();
    let (mut control, mut device, mut instance) = joined_stream(&relay).await?;
    let waiting_key = keypair()?;
    let mut waiting_control = register(&relay, &waiting_key).await?;
    let mut waiting_device = connect_device(&relay, &waiting_key).await?;
    let mut handshaking = open(&relay, REGISTER_PATH).await?;

    // Act
    relay.shutdown().await;
    let closes = [
        next_close(&mut control).await?,
        next_close(&mut device).await?,
        next_close(&mut instance).await?,
        next_close(&mut waiting_device).await?,
        next_close(&mut handshaking).await?,
    ];
    let rebound = TcpListener::bind(address).await;

    // Assert
    for close in closes {
        assert_eq!(close, (1001, "Relay stopped".to_string()));
    }
    assert!(rebound.is_ok());
    drop(waiting_control.close(None).await);

    Ok(())
}

#[tokio::test]
async fn dropping_the_handle_stops_the_relay() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let mut control = register(&relay, &keypair()?).await?;

    // Act
    drop(relay);
    let close = next_close(&mut control).await?;

    // Assert
    assert_eq!(close, (1001, "Relay stopped".to_string()));

    Ok(())
}
