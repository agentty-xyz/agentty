//! Pairing with host approval, token authentication, and handshake
//! rejection.

use std::time::Duration;

use ag_remote::protocol::{ClientFrame, RemoteRequest, ServerFrame};
use ag_remote::{PROTOCOL_VERSION, PairingRequest, RemoteConnection};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::support::{TestResult, TestServer, assert_rejected, hello_frame, next_frame, send};

#[tokio::test]
async fn paired_device_reconnects_with_its_token() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (client, device_id, token) = server.pair("Phone").await?;
    drop(client);

    // Act
    let (_client, welcome) = server.hello(&token).await?;

    // Assert
    assert_eq!(
        welcome,
        ServerFrame::Welcome {
            capabilities: Vec::new(),
            device_id: device_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            revision: 1,
        }
    );
    assert_eq!(server.devices.device_ids(), [device_id]);
    assert_ne!(server.devices.token_hashes(), [token]);
    assert_eq!(server.devices.last_seen(), [Some(200)]);

    Ok(())
}

#[tokio::test]
async fn status_lists_authenticated_connections_until_they_close() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let mut status = server.handle.status();

    // Act
    let (client, device_id, _token) = server.pair("Phone").await?;
    let connected = status
        .wait_for(|status| !status.connections.is_empty())
        .await?
        .clone();
    drop(client);
    let disconnected = status
        .wait_for(|status| status.connections.is_empty())
        .await?
        .clone();

    // Assert
    assert_eq!(
        connected.connections,
        [RemoteConnection {
            device_id,
            device_name: "Phone".to_string(),
        }]
    );
    assert_eq!(disconnected.connections, []);

    Ok(())
}

#[tokio::test]
async fn pairing_code_works_only_once() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let code = server.handle.issue_pairing_code()?.code;
    let pair = ClientFrame::Pair {
        code,
        device_name: "Phone".to_string(),
        protocol_version: PROTOCOL_VERSION,
    };
    let mut first = server.connect().await?;
    send(&mut first, &pair).await?;
    next_frame(&mut first).await?;
    let mut second = server.connect().await?;

    // Act
    send(&mut second, &pair).await?;

    // Assert
    assert_rejected(&mut second, "Invalid or expired pairing code").await?;
    assert_eq!(server.handle.status().borrow().pairing_requests.len(), 1);

    Ok(())
}

#[tokio::test]
async fn approved_pairing_lists_the_request_then_stores_the_device() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let code = server.handle.issue_pairing_code()?.code;
    let mut client = server.connect().await?;
    send(
        &mut client,
        &ClientFrame::Pair {
            code,
            device_name: "Phone".to_string(),
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    let ServerFrame::AwaitingApproval { confirmation } = next_frame(&mut client).await? else {
        return Err("pairing should wait for approval".into());
    };
    let mut status = server.handle.status();
    let pending = status
        .wait_for(|status| !status.pairing_requests.is_empty())
        .await?
        .pairing_requests
        .clone();

    // Act
    let approved = server.handle.approve_pairing(pending[0].id);
    let paired = next_frame(&mut client).await?;

    // Assert
    assert_eq!(
        pending,
        [PairingRequest {
            confirmation: confirmation.clone(),
            device_name: "Phone".to_string(),
            id: pending[0].id,
        }]
    );
    assert_eq!(confirmation.len(), 4);
    assert!(approved);
    let ServerFrame::Paired { device_id, .. } = paired else {
        return Err(format!("expected a paired frame, got {paired:?}").into());
    };
    assert_eq!(server.devices.device_ids(), [device_id]);
    let settled = status
        .wait_for(|status| status.pairing_requests.is_empty() && !status.connections.is_empty())
        .await?
        .clone();
    assert_eq!(settled.connections[0].device_name, "Phone");
    assert!(!server.handle.approve_pairing(pending[0].id));

    Ok(())
}

#[tokio::test]
async fn denied_pairing_is_rejected_without_a_device() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, request_id) = server.request_pairing("Phone").await?;

    // Act
    let denied = server.handle.deny_pairing(request_id);

    // Assert
    assert!(denied);
    assert_rejected(&mut client, "Pairing was denied").await?;
    assert_eq!(server.devices.device_ids(), Vec::<String>::new());
    assert!(!server.handle.deny_pairing(request_id));

    Ok(())
}

#[tokio::test]
async fn unanswered_pairing_times_out() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let started = tokio::time::Instant::now();
    let (mut client, _request_id) = server.request_pairing("Phone").await?;
    // Pausing after the request is listed keeps the clock from skipping
    // past the pairing round trips while loopback I/O is in flight.
    tokio::time::pause();

    // Act
    // Reads without a client-side timeout, so the paused clock advances
    // straight to the server's approval deadline.
    let message = client.next().await;

    // Assert
    let Some(Ok(Message::Text(text))) = message else {
        return Err(format!("expected a rejection frame, got {message:?}").into());
    };
    assert_eq!(
        serde_json::from_str::<ServerFrame>(&text)?,
        ServerFrame::Rejected {
            reason: "Pairing was not approved in time".to_string(),
        }
    );
    assert!(started.elapsed() >= Duration::from_mins(2));
    tokio::time::resume();
    server
        .handle
        .status()
        .wait_for(|status| status.pairing_requests.is_empty())
        .await?;
    assert_eq!(server.devices.device_ids(), Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn leaving_while_waiting_withdraws_the_request() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (client, request_id) = server.request_pairing("Phone").await?;
    let mut status = server.handle.status();

    // Act
    drop(client);

    // Assert
    status
        .wait_for(|status| status.pairing_requests.is_empty())
        .await?;
    assert!(!server.handle.approve_pairing(request_id));

    Ok(())
}

#[tokio::test]
async fn frames_sent_while_waiting_are_rejected() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _request_id) = server.request_pairing("Phone").await?;

    // Act
    send(
        &mut client,
        &ClientFrame::Request {
            id: 1,
            request: RemoteRequest::ListSessions,
        },
    )
    .await?;

    // Assert
    assert_rejected(&mut client, "Wait for pairing approval").await?;
    server
        .handle
        .status()
        .wait_for(|status| status.pairing_requests.is_empty())
        .await?;

    Ok(())
}

#[tokio::test]
async fn unknown_tokens_and_other_versions_are_rejected() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let mut unknown = server.connect().await?;
    let mut outdated = server.connect().await?;

    // Act
    send(&mut unknown, &hello_frame("not-a-token")).await?;
    send(
        &mut outdated,
        &ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION + 1,
            token: "t".to_string(),
        },
    )
    .await?;

    // Assert
    assert_rejected(&mut unknown, "Unknown device token").await?;
    assert_rejected(&mut outdated, "Unsupported protocol version").await
}

#[tokio::test]
async fn first_frame_must_be_hello_or_pair() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let mut request_first = server.connect().await?;
    let mut invalid_json = server.connect().await?;
    let mut binary = server.connect().await?;

    // Act
    send(
        &mut request_first,
        &ClientFrame::Request {
            id: 1,
            request: RemoteRequest::ListSessions,
        },
    )
    .await?;
    invalid_json.send(Message::Text("{".into())).await?;
    binary.send(Message::Binary(vec![1, 2].into())).await?;

    // Assert
    assert_rejected(&mut request_first, "Authenticate before sending requests").await?;
    assert_rejected(&mut invalid_json, "Expected a hello or pair frame").await?;
    assert_rejected(&mut binary, "Expected a hello or pair frame").await
}

#[tokio::test]
async fn silent_clients_are_rejected_after_the_handshake_timeout() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let started = tokio::time::Instant::now();
    let mut client = server.connect().await?;
    // Pausing after the upgrade keeps the clock from skipping ahead while
    // the connection is still being set up.
    tokio::time::pause();

    // Act
    // Reads without a client-side timeout, so the paused clock advances
    // straight to the server's handshake deadline.
    let message = client.next().await;

    // Assert
    let Some(Ok(Message::Text(text))) = message else {
        return Err(format!("expected a rejection frame, got {message:?}").into());
    };
    assert_eq!(
        serde_json::from_str::<ServerFrame>(&text)?,
        ServerFrame::Rejected {
            reason: "Authentication timed out".to_string(),
        }
    );
    assert!(started.elapsed() >= Duration::from_secs(10));

    Ok(())
}
