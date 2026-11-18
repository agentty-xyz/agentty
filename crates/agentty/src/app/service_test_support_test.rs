//! Test support for `AppServices`: the scripted `TestSessionRunFactory`,
//! dependency setters, and an offline channel that aborts on model calls.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnRequest,
    TurnResult,
};
use ag_forge::ReviewRequestClient;
use ag_git::GitClient;
use ag_protocol::{AgentResponse, TurnPrompt};
use ag_worker::{RunClient, SessionRunClient};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{AppServices, SessionRunFactory};
use crate::domain::agent::AgentKind;
use crate::domain::session::SessionId;
use crate::infra::agent_profile::AgentCatalogClient;
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
    fn create_graph_task(
        &self,
        context_id: &str,
        _session_id: &SessionId,
        kind: AgentKind,
    ) -> Result<SessionRunClient, AgentError> {
        Ok(self.create(&SessionId::from(context_id), kind))
    }

    fn create_task(
        &self,
        context: &ag_session::TaskAgentContext,
    ) -> Result<SessionRunClient, AgentError> {
        Ok(self.create(
            &SessionId::from(context.task_id.as_str()),
            context.assignment.agent.kind(),
        ))
    }

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

    /// Scripted channels replace provider CLIs, so workspaces need no setup.
    fn setup_backend(&self, _kind: AgentKind, _folder: &Path) -> Result<(), AgentError> {
        Ok(())
    }
}

impl AppServices {
    /// Replaces session runtime composition, including workspace setup.
    pub(crate) fn set_session_run_factory(&mut self, factory: Arc<dyn SessionRunFactory>) {
        self.session_run_factory = factory;
    }

    pub(crate) fn set_graph_proposal_repository(
        &mut self,
        repository: Arc<dyn ag_store::GraphProposalRepository>,
    ) {
        self.repositories = self.repositories.with_graph_proposal_repository(repository);
    }

    /// Replaces workspace definition lookup independently of task persistence.
    pub(crate) fn set_agent_profile_catalog_client(&mut self, client: Arc<dyn AgentCatalogClient>) {
        self.agent_profile_catalog_client = client;
    }

    /// Injects task receipts independently of the remaining database ports.
    pub(crate) fn set_task_repository(&mut self, repository: Arc<dyn ag_store::TaskRepository>) {
        self.repositories = self.repositories.with_task_repository(repository);
    }

    /// Injects graph lifecycle persistence independently of worker dispatch.
    pub(crate) fn set_graph_repository(&mut self, repository: Arc<dyn ag_store::GraphRepository>) {
        self.repositories = self.repositories.with_graph_repository(repository);
    }

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

/// Session channel whose turns run until [`Self::release`] fires and which
/// records accepted steering prompts; prompts starting with `reject` are
/// refused, and ones starting with `late` are refused once
/// [`Self::late_steer`] fires.
#[derive(Default)]
pub(crate) struct SteerableSessionChannel {
    /// Answers the steering prompts that start with `late`.
    pub(crate) late_steer: CancellationToken,
    /// Completes every waiting and later turn with a plain answer.
    pub(crate) release: CancellationToken,
    /// Steering prompt texts in delivery order.
    pub(crate) steered: Mutex<Vec<String>>,
}

impl AgentChannel for SteerableSessionChannel {
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
        _session_id: String,
        _req: TurnRequest,
        _events: mpsc::UnboundedSender<TurnEvent>,
    ) -> AgentFuture<Result<TurnResult, AgentError>> {
        let release = self.release.clone();

        Box::pin(async move {
            release.cancelled().await;

            Ok(TurnResult {
                assistant_message: AgentResponse::plain("done"),
                context_reset: false,
                harness_report: None,
                input_tokens: 0,
                output_tokens: 0,
                provider_conversation_id: None,
            })
        })
    }

    fn steer_turn(
        &self,
        _session_id: String,
        prompt: TurnPrompt,
    ) -> AgentFuture<Result<(), AgentError>> {
        if prompt.text.starts_with("reject") {
            return Box::pin(async { Err(AgentError::Runtime("turn ended".to_string())) });
        }
        if prompt.text.starts_with("late") {
            let late_steer = self.late_steer.clone();

            return Box::pin(async move {
                late_steer.cancelled().await;

                Err(AgentError::Runtime("turn ended".to_string()))
            });
        }
        self.steered.lock().expect("steered").push(prompt.text);

        Box::pin(async { Ok(()) })
    }

    fn shutdown_session(&self, _session_id: String) -> AgentFuture<Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
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
