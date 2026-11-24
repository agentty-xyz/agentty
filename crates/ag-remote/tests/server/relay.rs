//! Devices that reach the server through a relay, end to end encrypted.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ag_noise::{Handshake, Session};
use ag_relay::{AdmissionError, AdmissionPolicy, CONNECT_PATH_PREFIX, RelayConfig, RelayHandle};
use ag_remote::protocol::{
    ClientFrame, RemoteImage, RemoteRequest, RemoteResponse, RemoteResult, ServerFrame,
};
use ag_remote::{
    PROTOCOL_VERSION, RelayStatus, RemoteRelayConfig, RemoteServerStatus, StaticKeypair,
};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::support::{
    Client, TestResult, TestServer, relay_limits, start_linked_server, start_relay,
    start_relay_with_admission, start_server_linked_to,
};

/// Largest frame the test device accepts, in bytes.
const MAX_FRAME_BYTES: usize = 1 << 20;
/// Longest wait for an expected message before a test fails.
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);

/// A device's encrypted stream to the server through the relay.
struct RelayDevice {
    session: Session,
    socket: Client,
}

impl RelayDevice {
    /// Connects to `instance` through `relay` and runs the device handshake.
    async fn connect(
        relay: &RelayHandle,
        instance: &StaticKeypair,
        device: &StaticKeypair,
    ) -> TestResult<Self> {
        let url = format!(
            "ws://{}{CONNECT_PATH_PREFIX}{}",
            relay.local_addr(),
            instance.public().fingerprint()
        );
        let (mut socket, _) = connect_async(url).await?;
        let mut handshake = Handshake::device_initiator(device, instance.public())?;
        socket
            .send(Message::Binary(handshake.write_message()?.into()))
            .await?;
        let second = next_binary(&mut socket).await?;
        handshake.read_message(&second)?;
        let session = handshake.into_session(MAX_FRAME_BYTES)?;

        Ok(Self { session, socket })
    }

    /// Encrypts and sends one frame.
    async fn send(&mut self, frame: &ClientFrame) -> TestResult {
        let text = serde_json::to_string(frame)?;
        for message in self.session.seal(text.as_bytes())? {
            self.socket.send(Message::Binary(message.into())).await?;
        }

        Ok(())
    }

    /// Receives and decrypts the next frame.
    async fn next_frame(&mut self) -> TestResult<ServerFrame> {
        loop {
            let message = next_binary(&mut self.socket).await?;
            if let Some(frame) = self.session.open(&message)? {
                return Ok(serde_json::from_slice(&frame)?);
            }
        }
    }
}

/// Returns the next binary message, skipping keepalive traffic.
async fn next_binary(socket: &mut Client) -> TestResult<Vec<u8>> {
    loop {
        let message = tokio::time::timeout(MESSAGE_TIMEOUT, socket.next())
            .await?
            .ok_or("stream ended")??;
        match message {
            Message::Binary(bytes) => return Ok(bytes.to_vec()),
            Message::Ping(_) | Message::Pong(_) => {}
            other => return Err(format!("expected a binary message, got {other:?}").into()),
        }
    }
}

/// Waits until `socket` is closed or ends, and fails on a timeout, so a
/// stream the server keeps open cannot pass for a dropped one.
async fn stream_ends(socket: &mut Client) -> TestResult<bool> {
    loop {
        match tokio::time::timeout(MESSAGE_TIMEOUT, socket.next()).await? {
            Some(Ok(Message::Close(_)) | Err(_)) | None => return Ok(true),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(other)) => return Err(format!("unexpected message {other:?}").into()),
        }
    }
}

#[tokio::test]
async fn relay_device_pairs_requests_and_reconnects_with_its_key() -> TestResult {
    // Arrange
    let relay = start_relay().await?;
    let instance = StaticKeypair::generate()?;
    let device_key = StaticKeypair::generate()?;
    let server = start_linked_server(&relay, &instance).await?;
    let code = server.handle.issue_pairing_code()?.code;

    // Act: pair through the relay.
    let mut device = RelayDevice::connect(&relay, &instance, &device_key).await?;
    device
        .send(&ClientFrame::Pair {
            code,
            device_name: "Phone".to_string(),
            protocol_version: PROTOCOL_VERSION,
        })
        .await?;
    let awaiting = device.next_frame().await?;
    let request_id = server
        .handle
        .status()
        .wait_for(|status| !status.pairing_requests.is_empty())
        .await?
        .pairing_requests[0]
        .id;
    server.handle.approve_pairing(request_id);
    let paired = device.next_frame().await?;
    device
        .send(&ClientFrame::Request {
            id: 1,
            request: RemoteRequest::ListSessions,
        })
        .await?;
    let listed = device.next_frame().await?;
    drop(device);

    // Act: reconnect with the same key and no token.
    let mut returning = RelayDevice::connect(&relay, &instance, &device_key).await?;
    returning
        .send(&ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: String::new(),
        })
        .await?;
    let welcomed = returning.next_frame().await?;
    let mut stranger = RelayDevice::connect(&relay, &instance, &StaticKeypair::generate()?).await?;
    stranger
        .send(&ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: String::new(),
        })
        .await?;
    let refused = stranger.next_frame().await?;

    // Assert
    assert!(matches!(awaiting, ServerFrame::AwaitingApproval { .. }));
    let ServerFrame::Paired {
        device_id, token, ..
    } = paired
    else {
        return Err(format!("expected a paired frame, got {paired:?}").into());
    };
    assert_eq!(token, "");
    assert!(matches!(
        listed,
        ServerFrame::Response {
            id: 1,
            result: RemoteResult::Ok(_)
        }
    ));
    assert!(matches!(
        welcomed,
        ServerFrame::Welcome { device_id: ref id, .. } if *id == device_id
    ));
    assert_eq!(
        refused,
        ServerFrame::Rejected {
            reason: "Unknown device token".to_string()
        }
    );
    assert_eq!(server.devices.token_hashes().len(), 1);

    Ok(())
}

#[tokio::test]
async fn broken_device_handshake_is_dropped_and_others_still_connect() -> TestResult {
    // Arrange
    let relay = start_relay().await?;
    let instance = StaticKeypair::generate()?;
    let server = start_linked_server(&relay, &instance).await?;
    let url = format!(
        "ws://{}{CONNECT_PATH_PREFIX}{}",
        relay.local_addr(),
        instance.public().fingerprint()
    );

    // Act
    let (mut broken, _) = connect_async(url).await?;
    broken.send(Message::Binary(vec![1, 2, 3].into())).await?;
    let broken_ended = stream_ends(&mut broken).await?;
    let mut device = RelayDevice::connect(&relay, &instance, &StaticKeypair::generate()?).await?;
    device
        .send(&ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: String::new(),
        })
        .await?;
    let answered = device.next_frame().await?;

    // Assert
    assert!(broken_ended);
    assert_eq!(
        answered,
        ServerFrame::Rejected {
            reason: "Unknown device token".to_string()
        }
    );
    server.handle.shutdown().await;

    Ok(())
}

#[tokio::test]
async fn server_reconnects_after_the_relay_restarts() -> TestResult {
    // Arrange
    let relay = start_relay().await?;
    let address = relay.local_addr();
    let instance = StaticKeypair::generate()?;
    let server = start_linked_server(&relay, &instance).await?;
    let mut status = server.handle.status();

    // Act
    relay.shutdown().await;
    tokio::time::timeout(
        MESSAGE_TIMEOUT,
        status.wait_for(|status| status.relay == Some(RelayStatus::Connecting)),
    )
    .await??;
    // Stay down long enough for at least one reconnect attempt to fail.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let restarted = ag_relay::start(RelayConfig {
        bind_addr: address,
        client_address_header: None,
        limits: relay_limits(),
    })
    .await?;
    let reconnected = tokio::time::timeout(
        Duration::from_secs(15),
        status.wait_for(|status| status.relay == Some(RelayStatus::Connected)),
    )
    .await;
    let reached = RelayDevice::connect(&restarted, &instance, &StaticKeypair::generate()?).await;

    // Assert
    assert!(reconnected.is_ok());
    assert!(reached.is_ok());
    server.handle.shutdown().await;

    Ok(())
}

/// What [`SwitchableAdmission`] answers.
#[derive(Clone, Copy)]
enum Decision {
    Admit,
    Fail,
    Refuse,
}

/// Admission policy whose answer the test changes between registrations.
struct SwitchableAdmission(Mutex<Decision>);

impl SwitchableAdmission {
    /// Makes later registrations get `decision`.
    fn set(&self, decision: Decision) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = decision;
    }
}

#[async_trait]
impl AdmissionPolicy for SwitchableAdmission {
    async fn admit(&self, _instance_id: &str) -> Result<bool, AdmissionError> {
        match *self.0.lock().unwrap_or_else(PoisonError::into_inner) {
            Decision::Admit => Ok(true),
            Decision::Fail => Err(AdmissionError("policy unavailable".to_string())),
            Decision::Refuse => Ok(false),
        }
    }
}

#[tokio::test]
async fn refusal_is_reported_only_while_the_relay_refuses_and_the_link_keeps_retrying() -> TestResult
{
    // Arrange
    let admission = Arc::new(SwitchableAdmission(Mutex::new(Decision::Refuse)));
    let relay =
        start_relay_with_admission(Arc::clone(&admission) as Arc<dyn AdmissionPolicy>).await?;
    let server = start_server_linked_to(&relay, &StaticKeypair::generate()?).await?;
    let status = server.handle.status();
    let reaches = |status: &watch::Receiver<RemoteServerStatus>, expected: RelayStatus| {
        let mut status = status.clone();

        async move {
            tokio::time::timeout(
                Duration::from_secs(10),
                status.wait_for(|status| status.relay == Some(expected)),
            )
            .await
            .is_ok()
        }
    };

    // Act: refused, then failing for another reason, then admitted.
    let refused = reaches(&status, RelayStatus::NotAdmitted).await;
    admission.set(Decision::Fail);
    let failed = reaches(&status, RelayStatus::Connecting).await;
    admission.set(Decision::Admit);
    let admitted = reaches(&status, RelayStatus::Connected).await;
    server.handle.shutdown().await;

    // Assert
    assert!(refused);
    assert!(failed);
    assert!(admitted);

    Ok(())
}

#[tokio::test]
async fn unreachable_relay_keeps_the_local_listener_working() -> TestResult {
    // Arrange: nothing listens on the relay address.
    let unused = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = unused.local_addr()?;
    drop(unused);
    let server = TestServer::start_linked(RemoteRelayConfig {
        instance_key: StaticKeypair::generate()?,
        url: format!("ws://{address}"),
    })
    .await?;

    // Act
    let status = server
        .handle
        .status()
        .wait_for(|status| status.relay.is_some())
        .await?
        .relay;
    let (_client, device_id, _token) = server.pair("Laptop").await?;
    server.handle.shutdown().await;

    // Assert
    assert_eq!(status, Some(RelayStatus::Connecting));
    assert_ne!(device_id, "");

    Ok(())
}

#[tokio::test]
async fn relay_device_sends_a_message_with_megabytes_of_images() -> TestResult {
    // Arrange
    let relay = start_relay().await?;
    let instance = StaticKeypair::generate()?;
    let server = start_linked_server(&relay, &instance).await?;
    let code = server.handle.issue_pairing_code()?.code;
    let mut device = RelayDevice::connect(&relay, &instance, &StaticKeypair::generate()?).await?;
    device
        .send(&ClientFrame::Pair {
            code,
            device_name: "Phone".to_string(),
            protocol_version: PROTOCOL_VERSION,
        })
        .await?;
    device.next_frame().await?;
    let request_id = server
        .handle
        .status()
        .wait_for(|status| !status.pairing_requests.is_empty())
        .await?
        .pairing_requests[0]
        .id;
    server.handle.approve_pairing(request_id);
    device.next_frame().await?;
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.resize(4 << 20, 0);

    // Act
    device
        .send(&ClientFrame::Request {
            id: 1,
            request: RemoteRequest::SendMessage {
                images: vec![RemoteImage {
                    data: STANDARD.encode(png),
                }],
                message: "Look".to_string(),
                session_id: "session-1".into(),
            },
        })
        .await?;
    let sent = device.next_frame().await?;

    // Assert
    assert_eq!(
        sent,
        ServerFrame::Response {
            id: 1,
            result: RemoteResult::Ok(RemoteResponse::Accepted),
        }
    );
    assert_eq!(
        server.backend.calls().last().map(String::as_str),
        Some("send:session-1:Look:png=4194304")
    );
    server.handle.shutdown().await;

    Ok(())
}
