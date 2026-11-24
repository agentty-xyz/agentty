//! Runtime composition for worker-owned session turns and pooled utility
//! execution.

use std::path::PathBuf;
use std::sync::Arc;

use ag_contracts::{
    AgentChannel, AgentError, AgentFuture, OneShotClient, OneShotError, OneShotRequest,
    OneShotSubmission, SessionCommandClient, SessionCommandOutput, SessionCommandRequest,
    TurnEvent, TurnRequest, TurnResult,
};
use ag_protocol::TurnPrompt;
use ag_session::AgentKind;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Provider composition configuration. Constructing it starts no processes.
#[derive(Clone, Default)]
pub struct RuntimeFactory {
    app_server: Option<Arc<dyn ag_agent::AppServerClient>>,
    native_harness: Option<ag_agent::NativeHarnessConfig>,
}

impl RuntimeFactory {
    /// Enables in-process [`AgentKind::Harness`] runtimes that keep each
    /// session's durable history in `<data_root>/<session id>/`. Without
    /// this, harness turns fail before execution.
    #[must_use]
    pub fn with_native_harness(mut self, data_root: PathBuf) -> Self {
        self.native_harness = Some(ag_agent::NativeHarnessConfig::new(data_root));

        self
    }

    /// Makes in-process [`AgentKind::Harness`] session turns copy from the
    /// per-project Cargo caches under `cache_root`; call it after
    /// [`Self::with_native_harness`], without which it has no effect.
    #[must_use]
    pub fn with_trusted_cargo_cache(mut self, cache_root: PathBuf) -> Self {
        self.native_harness = self
            .native_harness
            .map(|config| config.with_trusted_cargo_cache(cache_root));

        self
    }

    /// Composes a fresh runtime for one worker-owned session.
    pub fn session(&self, kind: AgentKind) -> SessionRuntime {
        SessionRuntime {
            channel: ag_agent::create_agent_channel(
                kind,
                self.app_server.clone(),
                self.native_harness.as_ref(),
            ),
        }
    }

    /// Composes the client that runs host commands in session sandboxes.
    /// Without [`Self::with_native_harness`], every command fails before it
    /// runs, because the sandbox and its session directories live there.
    pub fn session_commands(&self) -> Arc<dyn SessionCommandClient> {
        Arc::new(SessionCommands {
            native_harness: self.native_harness.clone(),
        })
    }

    /// Composes pooled utility execution with adapter-owned cleanup.
    pub fn utility(&self) -> UtilityRuntime {
        let client = ag_agent::RealOneShotClient::pooled(self.app_server.clone());
        let client = match &self.native_harness {
            Some(config) => client.with_native_harness(config.clone()),
            None => client,
        };

        UtilityRuntime {
            client: Arc::new(client),
        }
    }
}

/// Session runtime whose concrete adapter never escapes to application code.
#[derive(Clone)]
pub struct SessionRuntime {
    channel: Arc<dyn AgentChannel>,
}

impl SessionRuntime {
    /// Dispatches an admitted turn to the selected harness adapter.
    pub fn run_turn(
        &self,
        id: String,
        request: TurnRequest,
        events: mpsc::UnboundedSender<TurnEvent>,
    ) -> AgentFuture<Result<TurnResult, AgentError>> {
        self.channel.run_turn(id, request, events)
    }

    /// Delivers one user prompt into the selected harness's running session
    /// turn; errors leave the prompt undelivered.
    pub fn steer_turn(
        &self,
        id: String,
        prompt: TurnPrompt,
    ) -> AgentFuture<Result<(), AgentError>> {
        self.channel.steer_turn(id, prompt)
    }

    /// Releases resources belonging to the selected harness session.
    pub fn shutdown(&self, id: String) -> AgentFuture<Result<(), AgentError>> {
        self.channel.shutdown_session(id)
    }
}

/// Host commands run in session sandboxes through the native harness
/// configuration.
struct SessionCommands {
    native_harness: Option<ag_agent::NativeHarnessConfig>,
}

impl SessionCommandClient for SessionCommands {
    fn run(
        &self,
        request: SessionCommandRequest,
    ) -> AgentFuture<Result<SessionCommandOutput, AgentError>> {
        let native_harness = self.native_harness.clone();

        Box::pin(async move {
            let native_harness = native_harness.ok_or_else(|| {
                AgentError::Runtime(
                    "Session commands need the in-process harness configuration.".to_string(),
                )
            })?;

            native_harness
                .run_session_command(request)
                .await
                .map_err(AgentError::Runtime)
        })
    }
}

/// Runtime-owned isolated execution and provider cleanup.
pub struct UtilityRuntime {
    client: Arc<dyn OneShotClient>,
}

impl UtilityRuntime {
    /// Runs one worker-admitted utility request and awaits cancellation
    /// cleanup.
    ///
    /// # Errors
    /// Returns the provider failure or cancellation diagnostic.
    pub async fn submit_cancellable(
        &self,
        request: OneShotRequest,
        cancellation: CancellationToken,
    ) -> Result<OneShotSubmission, OneShotError> {
        self.client.submit_cancellable(request, cancellation).await
    }

    /// Releases pooled runtimes after admitted utility work settles.
    pub async fn close(&self) {
        self.client.close().await;
    }

    /// Escalates cleanup after the worker's graceful shutdown deadline.
    pub fn force_shutdown(&self) {
        self.client.force_shutdown();
    }
}

#[cfg(feature = "test-utils")]
#[path = "test_support_test.rs"]
pub mod test_support;
