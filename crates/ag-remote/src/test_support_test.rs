//! Crate-wide fixtures for source tests: entropy, device store, and session
//! backend doubles plus server state builders.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use ag_session::{
    AgentModel, AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest,
    DurableMessageRequest, HostContext, HostProjectStatus, HostProjectSync, HostProjectSyncPhase,
    HostSessionMode, QuestionItem, ReasoningLevel, ResponseStyle, ReviewRequest, Session,
    SessionBackend, SessionDiffFile, SessionError, SessionFileEntry, SessionId, SessionMessage,
    SessionMessageKind, SessionOperation, SessionService, SessionSettings, SessionSettingsChange,
    SessionStatus, SessionSummary, SpeedMode,
};
use async_trait::async_trait;
use tokio::sync::watch;

use crate::device::{DeviceStore, DeviceStoreError, NewRemoteDevice, RemoteDevice};
use crate::entropy::Entropy;
use crate::server::{RemoteHost, ServerState};

/// Entropy that always fails.
pub(crate) struct FailingEntropy;

impl Entropy for FailingEntropy {
    fn fill(&self, _bytes: &mut [u8]) -> bool {
        false
    }
}

/// Entropy that replays `bytes` cyclically.
pub(crate) struct ReplayEntropy {
    bytes: Vec<u8>,
    position: Mutex<usize>,
}

impl ReplayEntropy {
    /// Creates entropy that yields `bytes` in order, then repeats them.
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            position: Mutex::new(0),
        }
    }
}

impl Entropy for ReplayEntropy {
    fn fill(&self, bytes: &mut [u8]) -> bool {
        let mut position = self.position.lock().expect("entropy position lock");
        for byte in bytes {
            *byte = self.bytes[*position % self.bytes.len()];
            *position += 1;
        }

        true
    }
}

/// Device store operation names that [`MemoryDeviceStore`] can fail.
pub(crate) const FIND: &str = "find";
pub(crate) const INSERT: &str = "insert";
pub(crate) const TOUCH: &str = "touch";

/// In-memory device store with injectable failures.
#[derive(Default)]
pub(crate) struct MemoryDeviceStore {
    devices: Mutex<Vec<(RemoteDevice, String)>>,
    failing: BTreeSet<&'static str>,
    touched: Mutex<Vec<String>>,
}

impl MemoryDeviceStore {
    /// Creates a store whose named operations fail.
    pub(crate) fn failing(operations: &[&'static str]) -> Self {
        Self {
            failing: operations.iter().copied().collect(),
            ..Self::default()
        }
    }

    /// Stores one device whose token hashes to `token_hash`.
    pub(crate) fn with_device(self, device_id: &str, token_hash: &str) -> Self {
        self.devices.lock().expect("devices lock").push((
            RemoteDevice {
                created_at: 1,
                id: device_id.to_string(),
                last_seen_at: None,
                name: format!("{device_id} name"),
            },
            token_hash.to_string(),
        ));

        self
    }

    /// Returns the stored device identifiers and token hashes.
    pub(crate) fn stored(&self) -> Vec<(String, String, String)> {
        self.devices
            .lock()
            .expect("devices lock")
            .iter()
            .map(|(device, hash)| (device.id.clone(), device.name.clone(), hash.clone()))
            .collect()
    }

    /// Returns the devices touched so far.
    pub(crate) fn touched(&self) -> Vec<String> {
        self.touched.lock().expect("touched lock").clone()
    }

    fn check(&self, operation: &str) -> Result<(), DeviceStoreError> {
        if self.failing.contains(operation) {
            return Err(DeviceStoreError(format!("{operation} failed")));
        }

        Ok(())
    }
}

#[async_trait]
impl DeviceStore for MemoryDeviceStore {
    async fn list_devices(&self) -> Result<Vec<RemoteDevice>, DeviceStoreError> {
        Ok(self
            .devices
            .lock()
            .expect("devices lock")
            .iter()
            .map(|(device, _)| device.clone())
            .collect())
    }

    async fn find_device_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<RemoteDevice>, DeviceStoreError> {
        self.check(FIND)?;

        Ok(self
            .devices
            .lock()
            .expect("devices lock")
            .iter()
            .find(|(_, hash)| hash == token_hash)
            .map(|(device, _)| device.clone()))
    }

    async fn insert_device(&self, device: NewRemoteDevice) -> Result<(), DeviceStoreError> {
        self.check(INSERT)?;
        self.devices.lock().expect("devices lock").push((
            RemoteDevice {
                created_at: 1,
                id: device.id,
                last_seen_at: None,
                name: device.name,
            },
            device.token_hash,
        ));

        Ok(())
    }

    async fn touch_device(&self, device_id: &str) -> Result<(), DeviceStoreError> {
        self.check(TOUCH)?;
        self.touched
            .lock()
            .expect("touched lock")
            .push(device_id.to_string());

        Ok(())
    }

    async fn delete_device(&self, device_id: &str) -> Result<(), DeviceStoreError> {
        self.devices
            .lock()
            .expect("devices lock")
            .retain(|(device, _)| device.id != device_id);

        Ok(())
    }
}

/// Session backend that records calls and answers every operation alike.
#[derive(Default)]
pub(crate) struct RecordingBackend {
    calls: Mutex<Vec<String>>,
    diff_files: Option<Vec<SessionDiffFile>>,
    is_failing: bool,
    session_files: Option<Vec<SessionFileEntry>>,
    supported: Vec<SessionOperation>,
}

impl RecordingBackend {
    /// Creates a backend that serves diffs and reports `files` as the
    /// changes of every session, or `None` as a session it has not loaded.
    pub(crate) fn with_diff(files: Option<Vec<SessionDiffFile>>) -> Self {
        Self {
            diff_files: files,
            supported: vec![SessionOperation::SessionDiff],
            ..Self::default()
        }
    }

    /// Creates a backend that serves file listings and reports `entries` as
    /// the worktree of every session, or `None` as a session it has not
    /// loaded.
    pub(crate) fn with_files(entries: Option<Vec<SessionFileEntry>>) -> Self {
        Self {
            session_files: entries,
            supported: vec![SessionOperation::SessionFiles],
            ..Self::default()
        }
    }

    /// Creates a backend that serves project file listings and reports
    /// `entries` as the active project's checkout.
    pub(crate) fn with_project_files(entries: Vec<SessionFileEntry>) -> Self {
        Self {
            session_files: Some(entries),
            supported: vec![SessionOperation::ProjectFiles],
            ..Self::default()
        }
    }

    /// Creates a backend that serves the optional `operations`.
    pub(crate) fn supporting(operations: &[SessionOperation]) -> Self {
        Self {
            supported: operations.to_vec(),
            ..Self::default()
        }
    }

    /// Creates a backend whose every operation fails.
    pub(crate) fn failing() -> Self {
        Self {
            is_failing: true,
            ..Self::default()
        }
    }

    /// Returns the recorded calls.
    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn record<Value>(&self, call: String, value: Value) -> Result<Value, SessionError> {
        self.calls.lock().expect("calls lock").push(call);
        if self.is_failing {
            return Err(SessionError::Operation("backend failed".to_string()));
        }

        Ok(value)
    }
}

#[async_trait]
impl SessionBackend for RecordingBackend {
    fn supports(&self, operation: SessionOperation) -> bool {
        self.supported.contains(&operation)
    }

    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.record(
            format!("create:{:?}:{}", request.mode, request.project_id),
            SessionId::from("created"),
        )
    }

    async fn host_context(&self) -> Result<HostContext, SessionError> {
        self.record("host".to_string(), host_fixture())
    }

    async fn project_status(&self) -> Result<HostProjectStatus, SessionError> {
        self.record("project".to_string(), project_status_fixture())
    }

    async fn sync_project(&self) -> Result<(), SessionError> {
        self.record("sync_project".to_string(), ())
    }

    async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError> {
        let session = (session_id.as_str() != "missing").then(|| session_fixture(session_id));

        self.record(format!("get:{session_id}"), session)
    }

    async fn get_session_diff(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionDiffFile>>, SessionError> {
        self.record(format!("diff:{session_id}"), self.diff_files.clone())
    }

    async fn list_session_files(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionFileEntry>>, SessionError> {
        self.record(format!("files:{session_id}"), self.session_files.clone())
    }

    async fn list_project_files(&self) -> Result<Vec<SessionFileEntry>, SessionError> {
        self.record(
            "project_files".to_string(),
            self.session_files.clone().unwrap_or_default(),
        )
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        self.record(
            "list".to_string(),
            vec![SessionSummary {
                id: SessionId::from("session-1"),
                status: SessionStatus::InProgress,
                title: Some("Build it".to_string()),
                updated_at: 20,
            }],
        )
    }

    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError> {
        self.record(format!("send:{session_id}:{message}"), ())
    }

    async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        _request: DurableMessageRequest,
    ) -> Result<(), SessionError> {
        self.record(format!("durable:{session_id}"), ())
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

        self.record(
            format!(
                "answer:{session_id}:{}:{}",
                answers.join(","),
                request.attachments.len()
            ),
            (),
        )
    }

    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("cancel:{session_id}"), ())
    }

    async fn stop_turn(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("stop:{session_id}"), ())
    }

    async fn rebase_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("sync:{session_id}"), ())
    }

    async fn update_session_settings(
        &self,
        session_id: &SessionId,
        change: SessionSettingsChange,
    ) -> Result<(), SessionError> {
        let model = change.model.map_or("-", AgentModel::as_str);
        let reasoning = change.reasoning_level.map_or("-", ReasoningLevel::as_str);
        let style = change.response_style.map_or("-", ResponseStyle::as_str);
        let speed = change.speed_mode.map_or("-", SpeedMode::as_str);

        self.record(
            format!("settings:{session_id}:{model}:{reasoning}:{style}:{speed}"),
            (),
        )
    }

    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("merge:{session_id}"), ())
    }

    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("review:{session_id}"));

        Err(SessionError::Operation("unsupported".to_string()))
    }
}

/// Describes project `1` on `main`, which serves regular sessions and
/// drafts but not the harness.
pub(crate) fn host_fixture() -> HostContext {
    HostContext {
        base_branch: "main".to_string(),
        default_model: ag_session::test_support::CODEX_MODEL,
        models: vec![ag_session::test_support::codex_selection()],
        project_id: 1,
        project_name: "agentty".to_string(),
        session_modes: vec![
            HostSessionMode {
                agent: ag_session::test_support::codex_selection(),
                mode: CreateSessionMode::Regular,
                reasoning_level: ReasoningLevel::High,
                response_style: ResponseStyle::Balanced,
                speed_mode: SpeedMode::Fast,
            },
            HostSessionMode {
                agent: ag_session::test_support::codex_selection(),
                mode: CreateSessionMode::Draft,
                reasoning_level: ReasoningLevel::High,
                response_style: ResponseStyle::Balanced,
                speed_mode: SpeedMode::Fast,
            },
        ],
    }
}

/// Describes project `agentty` on `main`, one commit ahead of and two
/// behind its upstream, while a sync resolves three conflicted files.
pub(crate) fn project_status_fixture() -> HostProjectStatus {
    HostProjectStatus {
        ahead_behind: Some((1, 2)),
        branch: Some("main".to_string()),
        project_name: "agentty".to_string(),
        sync: Some(HostProjectSync {
            branch: "main".to_string(),
            phase: HostProjectSyncPhase::ResolvingConflicts {
                conflicted_file_count: 3,
            },
        }),
    }
}

/// Builds a session with every transcript message kind and one question.
pub(crate) fn session_fixture(session_id: &SessionId) -> Session {
    Session {
        created_at: 10,
        draft_prompt: None,
        id: session_id.clone(),
        messages: vec![
            SessionMessage::new(0, SessionMessageKind::UserPrompt, "build it"),
            SessionMessage::new(1, SessionMessageKind::AgentPrompt, "hidden context"),
            SessionMessage::new(2, SessionMessageKind::ActivitySummary, "ran tests"),
            SessionMessage::new(3, SessionMessageKind::AssistantAnswer, "done"),
            SessionMessage::new(4, SessionMessageKind::WorkflowNotice, "committed"),
        ],
        progress: None,
        published_upstream_ref: None,
        questions: vec![QuestionItem {
            options: vec!["main".to_string(), "develop".to_string()],
            text: "Which branch?".to_string(),
        }],
        queued_messages: vec!["next".to_string()],
        review_request: None,
        settings: SessionSettings {
            agent: ag_session::test_support::codex_selection(),
            base_branch: "main".to_string(),
            is_draft: false,
            project_id: 7,
            reasoning_level: ReasoningLevel::High,
            response_style: ResponseStyle::Balanced,
            speed_mode: SpeedMode::Normal,
        },
        status: SessionStatus::Question,
        title: Some("Build it".to_string()),
        updated_at: 20,
    }
}

/// Server state wired to the given doubles, with revision `7`.
pub(crate) struct StateFixture {
    pub(crate) backend: Arc<RecordingBackend>,
    pub(crate) devices: Arc<MemoryDeviceStore>,
    pub(crate) revisions_tx: watch::Sender<u64>,
    pub(crate) state: Arc<ServerState>,
}

impl StateFixture {
    /// Builds state over `devices`, `backend`, and `entropy`.
    pub(crate) fn new(
        devices: MemoryDeviceStore,
        backend: RecordingBackend,
        entropy: Arc<dyn Entropy>,
    ) -> Self {
        let backend = Arc::new(backend);
        let devices = Arc::new(devices);
        let (revisions_tx, revisions_rx) = watch::channel(7);
        let shutdown_rx = watch::Sender::new(false).subscribe();
        let state = Arc::new(ServerState::new(
            RemoteHost {
                devices: Arc::clone(&devices) as Arc<dyn DeviceStore>,
                revisions: revisions_rx,
                sessions: SessionService::new(Arc::clone(&backend) as Arc<dyn SessionBackend>),
            },
            entropy,
            shutdown_rx,
            Vec::new(),
        ));

        Self {
            backend,
            devices,
            revisions_tx,
            state,
        }
    }
}
