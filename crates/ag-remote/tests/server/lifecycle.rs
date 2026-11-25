//! Device revocation, shutdown, and listener binding.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use ag_remote::protocol::{ClientFrame, RemoteRequest, ServerFrame};
use ag_remote::{DeviceStore, RemoteError, RemoteHost, RemoteServerConfig};
use ag_session::SessionService;
use tokio::sync::watch;

use crate::support::{
    MemoryDeviceStore, RecordingBackend, TestResult, TestServer, assert_rejected, close_frame,
    next_frame, send,
};

#[tokio::test]
async fn revoking_a_device_closes_its_connections_and_invalidates_its_token() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut paired, device_id, token) = server.pair("Phone").await?;
    let (mut other, _other_id, _other_token) = server.pair("Tablet").await?;
    let mut status = server.handle.status();

    // Act
    server.handle.revoke_device(&device_id).await?;

    // Assert
    assert_rejected(&mut paired, "Device revoked").await?;
    let (_retry, retry_frame) = server.hello(&token).await?;
    assert_eq!(
        retry_frame,
        ServerFrame::Rejected {
            reason: "Unknown device token".to_string(),
        }
    );
    let remaining = status
        .wait_for(|status| status.connections.len() == 1)
        .await?
        .clone();
    assert_eq!(remaining.connections[0].device_name, "Tablet");
    assert_eq!(server.devices.device_ids().len(), 1);
    send(
        &mut other,
        &ClientFrame::Request {
            id: 1,
            request: RemoteRequest::ListSessions { project_id: None },
        },
    )
    .await?;
    assert!(matches!(
        next_frame(&mut other).await?,
        ServerFrame::Response { id: 1, .. }
    ));

    Ok(())
}

#[tokio::test]
async fn client_close_handshake_ends_the_connection() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let mut status = server.handle.status();

    // Act
    client.close(None).await?;

    // Assert
    let disconnected = status
        .wait_for(|status| status.connections.is_empty())
        .await?
        .clone();
    assert_eq!(disconnected.connections, []);

    Ok(())
}

#[tokio::test]
async fn failed_revocation_keeps_the_device_connected() -> TestResult {
    // Arrange
    let server = TestServer::start_with(
        MemoryDeviceStore::failing_deletes(),
        RecordingBackend::default(),
    )
    .await?;
    let (_client, device_id, _token) = server.pair("Phone").await?;

    // Act
    let result = server.handle.revoke_device(&device_id).await;

    // Assert
    assert!(matches!(result, Err(RemoteError::DeviceStore(_))));
    assert_eq!(server.handle.status().borrow().connections.len(), 1);

    Ok(())
}

#[tokio::test]
async fn shutdown_closes_sockets_and_releases_the_port() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let addr = server.handle.local_addr();
    let (mut authenticated, _device_id, _token) = server.pair("Phone").await?;
    let mut handshaking = server.connect().await?;

    // Act
    server.handle.shutdown().await;

    // Assert
    let authenticated_close = close_frame(&mut authenticated)
        .await?
        .ok_or("authenticated socket should close")?;
    let handshaking_close = close_frame(&mut handshaking)
        .await?
        .ok_or("handshaking socket should close")?;
    assert_eq!(u16::from(authenticated_close.code), 1001);
    assert_eq!(u16::from(handshaking_close.code), 1001);
    assert_eq!(authenticated_close.reason.as_str(), "Server stopped");
    tokio::net::TcpListener::bind(addr).await?;

    Ok(())
}

#[tokio::test]
async fn shutdown_closes_sockets_waiting_for_pairing_approval() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut waiting, _request_id) = server.request_pairing("Phone").await?;

    // Act
    server.handle.shutdown().await;

    // Assert
    let close = close_frame(&mut waiting)
        .await?
        .ok_or("waiting socket should close")?;
    assert_eq!(u16::from(close.code), 1001);
    assert_eq!(server.devices.device_ids(), Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn dropping_the_handle_stops_the_server() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    drop(server);

    // Assert
    let close = close_frame(&mut client)
        .await?
        .ok_or("socket should close")?;
    assert_eq!(u16::from(close.code), 1001);

    Ok(())
}

#[tokio::test]
async fn start_reports_an_address_that_is_already_bound() -> TestResult {
    // Arrange
    let occupied =
        tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
    let (_revisions_tx, revisions) = watch::channel(0);
    let host = RemoteHost {
        devices: Arc::new(MemoryDeviceStore::default()) as Arc<dyn DeviceStore>,
        revisions,
        sessions: SessionService::new(Arc::new(RecordingBackend::default())),
    };

    // Act
    let result = ag_remote::start(
        RemoteServerConfig {
            allowed_hosts: Vec::new(),
            bind_addr: occupied.local_addr()?,
            relay: None,
        },
        host,
    )
    .await;

    // Assert
    assert!(matches!(result, Err(RemoteError::Bind(_))));

    Ok(())
}
