use std::sync::Arc;

use ag_contracts::{
    AgentChannel, AgentError, AgentFuture, OneShotClient, OneShotError, OneShotRequest,
    OneShotSubmission, TurnEvent, TurnRequest, TurnResult,
};
use ag_session::AgentKind;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Provider composition configuration. Constructing it starts no processes.
#[derive(Clone, Default)]
pub struct RuntimeFactory {
    app_server: Option<Arc<dyn ag_agent::AppServerClient>>,
}

impl RuntimeFactory {
    /// Composes a fresh runtime for one worker-owned session.
    pub fn session(&self, kind: AgentKind) -> SessionRuntime {
        SessionRuntime {
            channel: ag_agent::create_agent_channel(kind, self.app_server.clone()),
        }
    }

    /// Composes pooled utility execution with adapter-owned cleanup.
    pub fn utility(&self) -> UtilityRuntime {
        UtilityRuntime {
            client: Arc::new(ag_agent::RealOneShotClient::pooled(self.app_server.clone())),
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

    /// Releases resources belonging to the selected harness session.
    pub fn shutdown(&self, id: String) -> AgentFuture<Result<(), AgentError>> {
        self.channel.shutdown_session(id)
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
