//! Running-server fixture with in-memory doubles and a WebSocket client.

use std::error::Error;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use ag_relay::{RelayConfig, RelayHandle, RelayLimits};
use ag_remote::protocol::{ClientFrame, ServerFrame};
use ag_remote::{
    DeviceStore, DeviceStoreError, NewRemoteDevice, PROTOCOL_VERSION, RelayStatus, RemoteDevice,
    RemoteHost, RemoteRelayConfig, RemoteServerConfig, RemoteServerHandle, StaticKeypair,
};
use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, DurableMessageRequest, QuestionItem,
    ReviewRequest, Session, SessionBackend, SessionError, SessionId, SessionMessage,
    SessionMessageKind, SessionService, SessionSettings, SessionStatus, SessionSummary,
};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Client side of one remote control WebSocket.
pub(crate) type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Result type for tests and the helpers they share.
pub(crate) type TestResult<Value = ()> = Result<Value, Box<dyn Error>>;

/// Longest wait for an expected frame before a test fails.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// A started server with handles on its doubles.
pub(crate) struct TestServer {
    pub(crate) backend: Arc<RecordingBackend>,
    pub(crate) devices: Arc<MemoryDeviceStore>,
    pub(crate) handle: RemoteServerHandle,
    pub(crate) revisions_tx: watch::Sender<u64>,
}

impl TestServer {
    /// Starts a server on a free loopback port with revision `1`.
    pub(crate) async fn start() -> TestResult<Self> {
        Self::start_with(MemoryDeviceStore::default(), RecordingBackend::default()).await
    }

    /// Starts a server over `devices` and `backend`.
    pub(crate) async fn start_with(
        devices: MemoryDeviceStore,
        backend: RecordingBackend,
    ) -> TestResult<Self> {
        Self::start_configured(devices, backend, None).await
    }

    /// Starts a server that also registers with `relay`.
    pub(crate) async fn start_linked(relay: RemoteRelayConfig) -> TestResult<Self> {
        Self::start_configured(
            MemoryDeviceStore::default(),
            RecordingBackend::default(),
            Some(relay),
        )
        .await
    }

    /// Starts a server over `devices` and `backend`, linked to `relay` when
    /// given.
    async fn start_configured(
        devices: MemoryDeviceStore,
        backend: RecordingBackend,
        relay: Option<RemoteRelayConfig>,
    ) -> TestResult<Self> {
        let backend = Arc::new(backend);
        let devices = Arc::new(devices);
        let (revisions_tx, revisions) = watch::channel(1);
        let host = RemoteHost {
            devices: Arc::clone(&devices) as Arc<dyn DeviceStore>,
            revisions,
            sessions: SessionService::new(Arc::clone(&backend) as Arc<dyn SessionBackend>),
        };
        let handle = ag_remote::start(
            RemoteServerConfig {
                allowed_hosts: vec!["box.tailnet.ts.net".to_string()],
                bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                relay,
            },
            host,
        )
        .await?;

        Ok(Self {
            backend,
            devices,
            handle,
            revisions_tx,
        })
    }

    /// Opens a WebSocket without sending a frame.
    pub(crate) async fn connect(&self) -> TestResult<Client> {
        let url = format!("ws://{}/ws", self.handle.local_addr());
        let (client, _) = tokio_tungstenite::connect_async(url).await?;

        Ok(client)
    }

    /// Pairs a new device, approving it through the handle, and returns its
    /// open client, device id, and token.
    pub(crate) async fn pair(&self, device_name: &str) -> TestResult<(Client, String, String)> {
        let (mut client, request_id) = self.request_pairing(device_name).await?;
        if !self.handle.approve_pairing(request_id) {
            return Err("pairing request should be pending".into());
        }
        let ServerFrame::Paired {
            device_id, token, ..
        } = next_frame(&mut client).await?
        else {
            return Err("pairing should succeed".into());
        };

        Ok((client, device_id, token))
    }

    /// Redeems a fresh code and returns the waiting client with the id of
    /// its listed pairing request.
    pub(crate) async fn request_pairing(&self, device_name: &str) -> TestResult<(Client, u64)> {
        let code = self.handle.issue_pairing_code()?.code;
        let mut client = self.connect().await?;
        send(
            &mut client,
            &ClientFrame::Pair {
                code,
                device_name: device_name.to_string(),
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await?;
        let ServerFrame::AwaitingApproval { confirmation } = next_frame(&mut client).await? else {
            return Err("pairing should wait for approval".into());
        };
        let request_id = self
            .handle
            .status()
            .wait_for(|status| {
                status
                    .pairing_requests
                    .iter()
                    .any(|request| request.confirmation == confirmation)
            })
            .await?
            .pairing_requests
            .iter()
            .find(|request| request.confirmation == confirmation)
            .map(|request| request.id)
            .ok_or("pairing request should be listed")?;

        Ok((client, request_id))
    }

    /// Opens a connection that sends a hello with `token` and returns the
    /// server's first frame.
    pub(crate) async fn hello(&self, token: &str) -> TestResult<(Client, ServerFrame)> {
        let mut client = self.connect().await?;
        send(&mut client, &hello_frame(token)).await?;
        let frame = next_frame(&mut client).await?;

        Ok((client, frame))
    }
}

/// Starts a relay on a free loopback port.
pub(crate) async fn start_relay() -> TestResult<RelayHandle> {
    let relay = ag_relay::start(RelayConfig {
        bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        client_address_header: None,
        limits: relay_limits(),
    })
    .await?;

    Ok(relay)
}

/// Relay limits with frequent keepalive pings, so the server's handling of
/// them runs in every test.
pub(crate) fn relay_limits() -> RelayLimits {
    RelayLimits {
        ping_interval: Duration::from_millis(50),
        ..RelayLimits::default()
    }
}

/// Starts a server linked to `relay` and waits until it is registered.
pub(crate) async fn start_linked_server(
    relay: &RelayHandle,
    instance: &StaticKeypair,
) -> TestResult<TestServer> {
    let server = TestServer::start_linked(RemoteRelayConfig {
        instance_key: instance.clone(),
        url: format!("ws://{}/", relay.local_addr()),
    })
    .await?;
    let mut status = server.handle.status();
    tokio::time::timeout(
        FRAME_TIMEOUT,
        status.wait_for(|status| status.relay == Some(RelayStatus::Connected)),
    )
    .await??;

    Ok(server)
}

/// Builds a hello frame for the current protocol.
pub(crate) fn hello_frame(token: &str) -> ClientFrame {
    ClientFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: token.to_string(),
    }
}

/// Sends one JSON frame.
pub(crate) async fn send(client: &mut Client, frame: &ClientFrame) -> TestResult {
    let text = serde_json::to_string(frame)?;
    client.send(Message::Text(text.into())).await?;

    Ok(())
}

/// Waits for the next server frame, skipping control messages.
pub(crate) async fn next_frame(client: &mut Client) -> TestResult<ServerFrame> {
    loop {
        let message = tokio::time::timeout(FRAME_TIMEOUT, client.next())
            .await?
            .ok_or("socket closed before the next frame")??;
        if let Message::Text(text) = message {
            return Ok(serde_json::from_str(&text)?);
        }
    }
}

/// Waits for the server's close frame, or `None` when the socket ends
/// without one.
pub(crate) async fn close_frame(client: &mut Client) -> TestResult<Option<CloseFrame>> {
    loop {
        match tokio::time::timeout(FRAME_TIMEOUT, client.next()).await? {
            Some(Ok(Message::Close(frame))) => return Ok(frame),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return Ok(None),
        }
    }
}

/// Asserts that the server rejects with `reason` and then closes with a
/// policy-violation code.
pub(crate) async fn assert_rejected(client: &mut Client, reason: &str) -> TestResult {
    assert_eq!(
        next_frame(client).await?,
        ServerFrame::Rejected {
            reason: reason.to_string(),
        }
    );
    let close = close_frame(client)
        .await?
        .ok_or("server should send a close frame")?;
    assert_eq!(u16::from(close.code), 1008);
    assert_eq!(close.reason.as_str(), reason);

    Ok(())
}

/// Locks `mutex`, recovering its data if another test thread panicked.
fn locked<Value>(mutex: &Mutex<Value>) -> MutexGuard<'_, Value> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// In-memory device store with an optional delete failure.
#[derive(Default)]
pub(crate) struct MemoryDeviceStore {
    devices: Mutex<Vec<(RemoteDevice, String)>>,
    is_delete_failing: bool,
}

impl MemoryDeviceStore {
    /// Creates a store whose deletions fail.
    pub(crate) fn failing_deletes() -> Self {
        Self {
            is_delete_failing: true,
            ..Self::default()
        }
    }

    /// Returns the stored device identifiers.
    pub(crate) fn device_ids(&self) -> Vec<String> {
        locked(&self.devices)
            .iter()
            .map(|(device, _)| device.id.clone())
            .collect()
    }

    /// Returns the stored token hashes.
    pub(crate) fn token_hashes(&self) -> Vec<String> {
        locked(&self.devices)
            .iter()
            .map(|(_, hash)| hash.clone())
            .collect()
    }

    /// Returns the last-seen timestamps.
    pub(crate) fn last_seen(&self) -> Vec<Option<i64>> {
        locked(&self.devices)
            .iter()
            .map(|(device, _)| device.last_seen_at)
            .collect()
    }
}

#[async_trait]
impl DeviceStore for MemoryDeviceStore {
    async fn list_devices(&self) -> Result<Vec<RemoteDevice>, DeviceStoreError> {
        Ok(locked(&self.devices)
            .iter()
            .map(|(device, _)| device.clone())
            .collect())
    }

    async fn find_device_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<RemoteDevice>, DeviceStoreError> {
        Ok(locked(&self.devices)
            .iter()
            .find(|(_, hash)| hash == token_hash)
            .map(|(device, _)| device.clone()))
    }

    async fn insert_device(&self, device: NewRemoteDevice) -> Result<(), DeviceStoreError> {
        locked(&self.devices).push((
            RemoteDevice {
                created_at: 100,
                id: device.id,
                last_seen_at: None,
                name: device.name,
            },
            device.token_hash,
        ));

        Ok(())
    }

    async fn touch_device(&self, device_id: &str) -> Result<(), DeviceStoreError> {
        for (device, _) in locked(&self.devices).iter_mut() {
            if device.id == device_id {
                device.last_seen_at = Some(200);
            }
        }

        Ok(())
    }

    async fn delete_device(&self, device_id: &str) -> Result<(), DeviceStoreError> {
        if self.is_delete_failing {
            return Err(DeviceStoreError("delete failed".to_string()));
        }
        locked(&self.devices).retain(|(device, _)| device.id != device_id);

        Ok(())
    }
}

/// Session backend that records calls; a session snapshot queues the
/// messages sent to it, `busy` sessions reject messages, and
/// every cancellation reports a missing session.
#[derive(Default)]
pub(crate) struct RecordingBackend {
    calls: Mutex<Vec<String>>,
}

impl RecordingBackend {
    /// Returns the recorded calls.
    pub(crate) fn calls(&self) -> Vec<String> {
        locked(&self.calls).clone()
    }

    fn record(&self, call: String) {
        locked(&self.calls).push(call);
    }
}

#[async_trait]
impl SessionBackend for RecordingBackend {
    async fn create_session(
        &self,
        _request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.record("create".to_string());

        Err(SessionError::Operation("unsupported".to_string()))
    }

    async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError> {
        self.record(format!("get:{session_id}"));
        if session_id.as_str() == "missing" {
            return Ok(None);
        }

        let prefix = format!("send:{session_id}:");
        let sent = self
            .calls()
            .iter()
            .filter_map(|call| call.strip_prefix(&prefix).map(str::to_string))
            .collect();

        Ok(Some(session(session_id, sent)))
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        self.record("list".to_string());

        Ok(vec![SessionSummary {
            id: SessionId::from("session-1"),
            status: SessionStatus::Review,
            title: Some("Build it".to_string()),
            updated_at: 20,
        }])
    }

    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError> {
        self.record(format!("send:{session_id}:{message}"));
        if session_id.as_str() == "busy" {
            return Err(SessionError::Operation("Session is busy".to_string()));
        }

        Ok(())
    }

    async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        _request: DurableMessageRequest,
    ) -> Result<(), SessionError> {
        self.record(format!("durable:{session_id}"));

        Ok(())
    }

    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        let answers: Vec<String> = request
            .answers
            .iter()
            .map(|answer| format!("{}={}", answer.question, answer.answer))
            .collect();
        self.record(format!("answer:{session_id}:{}", answers.join(",")));

        Ok(())
    }

    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("cancel:{session_id}"));

        Err(SessionError::NotFound)
    }

    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("merge:{session_id}"));

        Ok(())
    }

    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        self.record(format!("review:{session_id}"));

        Err(SessionError::Operation("unsupported".to_string()))
    }
}

/// Builds a session with a hidden prompt, an answer, one question, and
/// `queued_messages` waiting behind its turn.
fn session(session_id: &SessionId, queued_messages: Vec<String>) -> Session {
    Session {
        created_at: 10,
        draft_prompt: None,
        id: session_id.clone(),
        messages: vec![
            SessionMessage::new(0, SessionMessageKind::UserPrompt, "build it"),
            SessionMessage::new(1, SessionMessageKind::AgentPrompt, "hidden"),
            SessionMessage::new(2, SessionMessageKind::AssistantAnswer, "which branch?"),
        ],
        published_upstream_ref: None,
        questions: vec![QuestionItem {
            options: vec!["main".to_string()],
            text: "Which branch?".to_string(),
        }],
        queued_messages,
        review_request: None,
        settings: SessionSettings {
            agent: ag_session::test_support::codex_selection(),
            base_branch: "main".to_string(),
            is_draft: false,
            project_id: 1,
            reasoning_level: ag_session::ReasoningLevel::High,
            response_style: ag_session::ResponseStyle::Balanced,
            speed_mode: ag_session::SpeedMode::Normal,
        },
        status: SessionStatus::Question,
        title: Some("Build it".to_string()),
        updated_at: 20,
    }
}
