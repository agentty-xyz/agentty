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
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, DurableMessageRequest,
    ForgeKind, HostContext, HostSessionMode, QuestionItem, ReviewRequest, ReviewRequestState,
    ReviewRequestSummary, Session, SessionBackend, SessionDiffFile, SessionDiffFileStatus,
    SessionError, SessionFileEntry, SessionId, SessionListEntry, SessionListStats, SessionMessage,
    SessionMessageKind, SessionOperation, SessionService, SessionSettings, SessionSettingsChange,
    SessionStatus, SessionSummary,
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

/// Session backend that records every call and serves every optional
/// operation. Each method's comment says how it answers; [`session`]
/// describes the listed sessions.
#[derive(Default)]
pub(crate) struct RecordingBackend {
    calls: Mutex<Vec<String>>,
    released: watch::Sender<bool>,
}

impl RecordingBackend {
    /// Returns the recorded calls.
    pub(crate) fn calls(&self) -> Vec<String> {
        locked(&self.calls).clone()
    }

    /// Lets every held `wait` message and publish, now and later, finish.
    pub(crate) fn release(&self) {
        self.released.send_replace(true);
    }

    fn record(&self, call: String) {
        locked(&self.calls).push(call);
    }
}

#[async_trait]
impl SessionBackend for RecordingBackend {
    fn supports(&self, _operation: SessionOperation) -> bool {
        true
    }

    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.record(format!("create:{:?}:{}", request.mode, request.project_id));

        Ok(SessionId::from("session-2"))
    }

    // The host serves only regular sessions in project `1`.
    async fn host_context(&self) -> Result<HostContext, SessionError> {
        self.record("host".to_string());

        Ok(HostContext {
            base_branch: "main".to_string(),
            default_model: ag_session::test_support::CODEX_MODEL,
            models: vec![ag_session::test_support::codex_selection()],
            project_id: 1,
            project_name: "agentty".to_string(),
            session_modes: vec![HostSessionMode {
                agent: ag_session::test_support::codex_selection(),
                mode: CreateSessionMode::Regular,
            }],
        })
    }

    // A snapshot queues the messages sent to its session; `missing` is
    // absent.
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

    // Every diff adds one line to `a.rs`.
    async fn get_session_diff(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionDiffFile>>, SessionError> {
        self.record(format!("diff:{session_id}"));

        Ok(Some(vec![SessionDiffFile {
            added_lines: 1,
            deleted_lines: 0,
            old_path: None,
            patch: "diff --git a/a.rs b/a.rs\n+new\n".to_string(),
            path: "a.rs".to_string(),
            status: SessionDiffFileStatus::Added,
        }]))
    }

    // Every worktree holds the directory `src` and the file `src/lib.rs`.
    async fn list_session_files(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionFileEntry>>, SessionError> {
        self.record(format!("files:{session_id}"));

        Ok(Some(vec![
            SessionFileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            SessionFileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            },
        ]))
    }

    /// Lists `session-2` with the figures of a running turn, and every
    /// other session without figures, as an older host lists them.
    async fn list_session_entries(&self) -> Result<Vec<SessionListEntry>, SessionError> {
        let summaries = self.list_sessions().await?;

        Ok(summaries
            .into_iter()
            .map(|summary| {
                let stats = (summary.id.as_str() == "session-2").then(|| SessionListStats {
                    added_lines: 64,
                    deleted_lines: 9,
                    has_merge_conflict: false,
                    in_progress_started_at: Some(100),
                    in_progress_total_seconds: 12,
                    model: ag_session::test_support::codex_selection().model(),
                    reasoning_level: ag_session::ReasoningLevel::High,
                    review_request: None,
                    size_label: "M".to_string(),
                });

                SessionListEntry { stats, summary }
            })
            .collect())
    }

    // Lists `session-1`, waiting on a question, and `session-2`, running a
    // turn. `session-2` counts as updated once for every message sent and
    // every answer given to any session, so a device sees another session
    // change.
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let plan_updated_at = self
            .calls()
            .iter()
            .filter(|call| call.starts_with("send:") || call.starts_with("answer:"))
            .fold(30, |updated_at, _| updated_at + 1);
        self.record("list".to_string());

        Ok(vec![
            SessionSummary {
                id: SessionId::from("session-1"),
                status: SessionStatus::Review,
                title: Some("Build it".to_string()),
                updated_at: 20,
            },
            SessionSummary {
                id: SessionId::from("session-2"),
                status: SessionStatus::InProgress,
                title: Some("Plan it".to_string()),
                updated_at: plan_updated_at,
            },
        ])
    }

    // The message `busy` is rejected, `panic` panics, and `wait` is held,
    // and recorded only once [`RecordingBackend::release`] is called.
    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError> {
        if message == "wait" {
            let mut released = self.released.subscribe();
            let _ = released.wait_for(|is_released| *is_released).await;
        }
        self.record(format!("send:{session_id}:{message}"));
        if message == "busy" {
            return Err(SessionError::Operation("Session is busy".to_string()));
        }
        if message == "panic" {
            std::panic::resume_unwind(Box::new("the test backend fails this message"));
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

    // Every cancellation reports a missing session.
    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("cancel:{session_id}"));

        Err(SessionError::NotFound)
    }

    // Every turn stop succeeds.
    async fn stop_turn(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("stop:{session_id}"));

        Ok(())
    }

    // Every settings change succeeds and records its reasoning level.
    async fn update_session_settings(
        &self,
        session_id: &SessionId,
        change: SessionSettingsChange,
    ) -> Result<(), SessionError> {
        let reasoning = change
            .reasoning_level
            .map_or("-", ag_session::ReasoningLevel::as_str);
        self.record(format!("settings:{session_id}:{reasoning}"));

        Ok(())
    }

    // Every sync is refused.
    async fn rebase_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("sync:{session_id}"));

        Err(SessionError::Operation(
            "Session cannot sync in status `Done`".to_string(),
        ))
    }

    // Every merge is refused.
    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("merge:{session_id}"));

        Err(SessionError::Operation(
            "Session is not ready to merge".to_string(),
        ))
    }

    // A publish is held like the message `wait`, and then opens pull
    // request `#7`.
    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        let mut released = self.released.subscribe();
        let _ = released.wait_for(|is_released| *is_released).await;
        self.record(format!("review:{session_id}"));

        Ok(ReviewRequest {
            last_refreshed_at: 30,
            summary: ReviewRequestSummary {
                display_id: "#7".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: format!("agentty/{session_id}"),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Build it".to_string(),
                web_url: "https://github.com/example/project/pull/7".to_string(),
            },
        })
    }
}

/// Builds a session with `queued_messages` waiting behind its turn.
/// `session-2` runs a turn after an activity summary and a notice; any
/// other session has a hidden prompt, an answer, and one question.
fn session(session_id: &SessionId, queued_messages: Vec<String>) -> Session {
    let is_running = session_id.as_str() == "session-2";
    let (messages, questions, status, title) = if is_running {
        (
            vec![
                SessionMessage::new(0, SessionMessageKind::UserPrompt, "plan it"),
                SessionMessage::new(
                    1,
                    SessionMessageKind::ActivitySummary,
                    "✓ Read plan.md\n✗ Run tests\n… Edit plan.md",
                ),
                SessionMessage::new(
                    2,
                    SessionMessageKind::WorkflowNotice,
                    "[Commit Error] hook failed",
                ),
            ],
            Vec::new(),
            SessionStatus::InProgress,
            "Plan it",
        )
    } else {
        (
            vec![
                SessionMessage::new(0, SessionMessageKind::UserPrompt, "build it"),
                SessionMessage::new(1, SessionMessageKind::AgentPrompt, "hidden"),
                SessionMessage::new(2, SessionMessageKind::AssistantAnswer, "which branch?"),
            ],
            vec![QuestionItem {
                options: vec!["main".to_string()],
                text: "Which branch?".to_string(),
            }],
            SessionStatus::Question,
            "Build it",
        )
    };

    Session {
        created_at: 10,
        draft_prompt: None,
        id: session_id.clone(),
        messages,
        progress: None,
        published_upstream_ref: None,
        questions,
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
        status,
        title: Some(title.to_string()),
        updated_at: 20,
    }
}
