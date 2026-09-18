use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnRequest,
    TurnResult,
};
use ag_forge::ReviewRequestClient;
use ag_git::GitClient;
use ag_worker::{RunClient, SessionRunClient};
use tokio::sync::mpsc;

use super::{AppServices, SessionRunFactory};
use crate::domain::agent::AgentKind;
use crate::domain::session::SessionId;
use crate::infra::clipboard_image::ClipboardImageClient;
use crate::infra::fs::FsClient;

/// Supplies explicitly scripted worker channels, with an offline fallback.
#[derive(Default)]
pub(crate) struct TestSessionRunFactory {
    channels: Mutex<HashMap<SessionId, SessionRunClient>>,
}

impl TestSessionRunFactory {
    /// Installs one shared registry before configuring any session scripts.
    pub(crate) fn install(services: &mut AppServices) -> Arc<Self> {
        let factory = Arc::new(Self::default());
        services.session_run_factory = factory.clone();

        factory
    }

    /// Registers a worker client backed by an injected transport.
    pub(crate) fn register_run(&self, session_id: &str, run: SessionRunClient) {
        self.channels
            .lock()
            .expect("session scripts")
            .insert(SessionId::from(session_id), run);
    }

    /// Registers the next worker channel without removing other session
    /// scripts.
    pub(crate) fn register(&self, session_id: &str, channel: Arc<dyn AgentChannel>) {
        self.channels
            .lock()
            .expect("session channel scripts poisoned")
            .insert(
                SessionId::from(session_id),
                SessionRunClient::from_channel(session_id.to_string(), channel),
            );
    }
}

impl SessionRunFactory for TestSessionRunFactory {
    fn create(&self, session_id: &SessionId, _kind: AgentKind) -> SessionRunClient {
        self.channels
            .lock()
            .expect("session channel scripts poisoned")
            .remove(session_id)
            .unwrap_or_else(|| {
                SessionRunClient::from_channel(
                    session_id.to_string(),
                    Arc::new(OfflineSessionChannel),
                )
            })
    }
}

impl AppServices {
    /// Replaces only the Git boundary, retaining utility execution ownership.
    pub(crate) fn set_git_client(&mut self, client: Arc<dyn GitClient>) {
        self.git_client = client;
    }

    /// Replaces only the forge boundary, retaining utility execution ownership.
    pub(crate) fn set_review_request_client(&mut self, client: Arc<dyn ReviewRequestClient>) {
        self.review_request_client = client;
    }

    /// Replaces only clipboard access, retaining the remaining shared
    /// dependencies.
    pub(crate) fn set_clipboard_image_client(&mut self, client: Arc<dyn ClipboardImageClient>) {
        self.clipboard_image_client = client;
    }

    /// Replaces only filesystem access, retaining the remaining shared
    /// dependencies.
    pub(crate) fn set_fs_client(&mut self, client: Arc<dyn FsClient>) {
        self.fs_client = client;
    }

    /// Supplies a mock utility submitter while retaining cleanup of existing
    /// runs.
    pub(crate) fn set_run_client(&mut self, client: Arc<dyn RunClient>) {
        self.run_client = client;
    }
}

/// Allows non-model worker operations without starting a provider process.
struct OfflineSessionChannel;

impl AgentChannel for OfflineSessionChannel {
    fn start_session(
        &self,
        req: StartSessionRequest,
    ) -> AgentFuture<Result<SessionRef, AgentError>> {
        Box::pin(async move {
            Ok(SessionRef {
                session_id: req.session_id,
            })
        })
    }

    fn run_turn(
        &self,
        session_id: String,
        _req: TurnRequest,
        _events: mpsc::UnboundedSender<TurnEvent>,
    ) -> AgentFuture<Result<TurnResult, AgentError>> {
        Box::pin(async move {
            // A detached Tokio task can swallow a panic, falsely passing its
            // test. Fail the test process instead; nextest isolates
            // each test in a process.
            let _ = writeln!(
                io::stderr(),
                "unexpected model execution for session {session_id}; inject a scripted session \
                 channel"
            );
            std::process::abort();
        })
    }

    fn shutdown_session(&self, _session_id: String) -> AgentFuture<Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }
}
