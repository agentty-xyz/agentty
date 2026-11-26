//! Device revocation and shutdown.

use std::time::Duration;

use ag_remote::RemoteError;
use ag_remote::protocol::{ClientFrame, RemoteRequest, ServerFrame};
use ag_remote::test_support::{MemoryDeviceStore, RecordingBackend};

use crate::support::{
    TestResult, TestServer, assert_rejected, closed, next_frame, open_device_stream, send,
};

#[tokio::test]
async fn revoking_a_device_closes_its_connections_and_forgets_its_key() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut paired, device_id) = server.pair("Phone").await?;
    let key = paired.key().clone();
    let (mut other, _other_id) = server.pair("Tablet").await?;
    let mut status = server.handle.status();

    // Act
    server.handle.revoke_device(&device_id).await?;

    // Assert
    assert_rejected(&mut paired, "Device revoked").await?;
    let (_retry, retry_frame) = server.hello(&key).await?;
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
    let (mut client, _device_id) = server.pair("Phone").await?;
    let mut status = server.handle.status();

    // Act
    client.close().await?;

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
    let (_client, device_id) = server.pair("Phone").await?;

    // Act
    let result = server.handle.revoke_device(&device_id).await;

    // Assert
    assert!(matches!(result, Err(RemoteError::DeviceStore(_))));
    assert_eq!(server.handle.status().borrow().connections.len(), 1);

    Ok(())
}

#[tokio::test]
async fn shutdown_closes_streams_and_leaves_the_relay() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut authenticated, _device_id) = server.pair("Phone").await?;
    let mut handshaking = server.connect().await?;

    // Act
    server.handle.shutdown().await;

    // Assert: the relay forgets the instance once its control connection
    // ends, and then refuses device streams toward it.
    assert!(closed(&mut authenticated).await?);
    assert!(closed(&mut handshaking).await?);
    let forgotten = tokio::time::timeout(Duration::from_secs(5), async {
        while open_device_stream(&server.relay, &server.instance)
            .await
            .is_ok()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(forgotten.is_ok());

    Ok(())
}

#[tokio::test]
async fn shutdown_closes_streams_waiting_for_pairing_approval() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut waiting, _request_id) = server.request_pairing("Phone").await?;

    // Act
    server.handle.shutdown().await;

    // Assert
    assert!(closed(&mut waiting).await?);
    assert_eq!(server.devices.device_ids(), Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn dropping_the_handle_stops_the_server() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id) = server.pair("Phone").await?;

    // Act
    drop(server.handle);

    // Assert
    assert!(closed(&mut client).await?);

    Ok(())
}
