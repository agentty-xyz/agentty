//! The relay link: registration, reconnection, admission, the frames a
//! device reads on the wire, and the streams the server answers.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ag_relay::{AdmissionError, AdmissionPolicy, RelayConfig, RelayLimits};
use ag_remote::protocol::{ClientFrame, ServerFrame};
use ag_remote::test_support::{MemoryDeviceStore, RecordingBackend};
use ag_remote::{PROTOCOL_VERSION, RelayStatus, RemoteServerStatus, StaticKeypair};
use async_trait::async_trait;
use futures_util::SinkExt;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;

use crate::support::{
    Client, TestResult, TestServer, next_frame, open_device_stream, relay_limits, relay_url, send,
    start_relay_limited, start_relay_with_admission, start_server_linked_to, stream_ends,
    wait_for_relay,
};

#[tokio::test]
async fn pairing_frames_on_the_wire_are_what_released_web_clients_read() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let code = server.handle.issue_pairing_code()?.code;
    let mut device = server.connect().await?;
    device
        .send_text(&format!(
            r#"{{"type":"pair","code":"{code}","device_name":"Phone","protocol_version":1}}"#
        ))
        .await?;

    // Act
    let awaiting: serde_json::Value = serde_json::from_str(&device.next_untimed_text().await?)?;
    let request_id = server
        .handle
        .status()
        .wait_for(|status| !status.pairing_requests.is_empty())
        .await?
        .pairing_requests[0]
        .id;
    server.handle.approve_pairing(request_id);
    let paired: serde_json::Value = serde_json::from_str(&device.next_untimed_text().await?)?;

    // Assert: the same fields, and no secret, as every release has sent.
    assert_eq!(awaiting["type"], "awaiting_approval");
    assert_eq!(awaiting["confirmation"].as_str().map(str::len), Some(4));
    let mut paired_fields: Vec<&str> = paired
        .as_object()
        .ok_or("paired frame should be an object")?
        .keys()
        .map(String::as_str)
        .collect();
    paired_fields.sort_unstable();
    assert_eq!(
        paired_fields,
        [
            "capabilities",
            "device_id",
            "protocol_version",
            "revision",
            "type"
        ]
    );
    assert_eq!(paired["type"], "paired");
    assert_eq!(paired["protocol_version"], PROTOCOL_VERSION);

    Ok(())
}

#[tokio::test]
async fn broken_device_handshake_is_dropped_and_others_still_connect() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;

    // Act
    let mut broken = open_device_stream(&server.relay, &server.instance).await?;
    broken.send(Message::Binary(vec![1, 2, 3].into())).await?;
    let broken_ended = stream_ends(&mut broken).await?;
    let mut device = server.connect().await?;
    send(
        &mut device,
        &ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    let answered = next_frame(&mut device).await?;

    // Assert
    assert!(broken_ended);
    assert_eq!(
        answered,
        ServerFrame::Rejected {
            reason: "Unknown device token".to_string()
        }
    );

    Ok(())
}

#[tokio::test]
async fn streams_beyond_the_unauthenticated_cap_wait_until_a_slot_frees() -> TestResult {
    // Arrange: a relay that gives up on unanswered streams quickly.
    let relay = start_relay_limited(RelayLimits {
        accept_timeout: Duration::from_millis(500),
        ..relay_limits()
    })
    .await?;
    let server = TestServer::start_on(
        relay,
        MemoryDeviceStore::default(),
        RecordingBackend::default(),
    )
    .await?;
    let mut waiting = Vec::new();
    for _ in 0..8 {
        waiting.push(server.connect().await?);
    }

    // Act
    let over_cap = server.connect().await;
    waiting.pop();
    let mut after_release = None;
    for _ in 0..50 {
        if let Ok(client) = server.connect().await {
            after_release = Some(client);
            break;
        }
    }

    // Assert
    assert!(over_cap.is_err());
    let mut after_release = after_release.ok_or("a freed slot should serve a device")?;
    send(
        &mut after_release,
        &ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    assert!(matches!(
        next_frame(&mut after_release).await?,
        ServerFrame::Rejected { .. }
    ));

    Ok(())
}

#[tokio::test]
async fn server_reconnects_after_the_relay_restarts() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let address = server.relay.local_addr();

    // Act
    server.relay.shutdown().await;
    wait_for_relay(&server.handle, RelayStatus::Connecting).await?;
    // Stay down long enough for at least one reconnect attempt to fail.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let restarted = ag_relay::start(RelayConfig {
        bind_addr: address,
        client_address_header: None,
        limits: relay_limits(),
    })
    .await?;
    let mut status = server.handle.status();
    let reconnected = tokio::time::timeout(
        Duration::from_secs(15),
        status.wait_for(|status| status.relay == RelayStatus::Connected),
    )
    .await;
    let reached = Client::connect(&restarted, &server.instance, StaticKeypair::generate()?).await;

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
    let handle = start_server_linked_to(
        relay_url(&relay),
        &StaticKeypair::generate()?,
        RecordingBackend::default(),
    );
    let status = handle.status();
    let reaches = |status: &watch::Receiver<RemoteServerStatus>, expected: RelayStatus| {
        let mut status = status.clone();

        async move {
            tokio::time::timeout(
                Duration::from_secs(10),
                status.wait_for(|status| status.relay == expected),
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
    handle.shutdown().await;

    // Assert
    assert!(refused);
    assert!(failed);
    assert!(admitted);

    Ok(())
}

#[tokio::test]
async fn unreachable_relay_keeps_the_link_connecting_and_pairing_codes_available() -> TestResult {
    // Arrange: nothing listens on the relay address.
    let unused = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = unused.local_addr()?;
    drop(unused);
    let handle = start_server_linked_to(
        format!("ws://{address}"),
        &StaticKeypair::generate()?,
        RecordingBackend::default(),
    );

    // Act
    // Stay long enough for at least one attempt to fail.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let status = handle.status().borrow().relay;
    let code = handle.issue_pairing_code();
    handle.shutdown().await;

    // Assert
    assert_eq!(status, RelayStatus::Connecting);
    assert!(code.is_ok());

    Ok(())
}
