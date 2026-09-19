//! Explicit adapter fixtures for offline worker integration tests.
use std::sync::Arc;

pub use ag_runtime::test_support::{
    AgentBackend, AgentBackendError, AppServerClient, AppServerError, AppServerTurnResponse,
    MockAgentBackend, MockAppServerClient, RealOneShotClient,
};
use ag_session::AgentKind;

use crate::{RuntimeConfig, SessionRunClient};

/// Creates a worker client backed by a scripted CLI transport.
pub fn cli_session(
    session_id: String,
    backend: Arc<dyn AgentBackend>,
    kind: AgentKind,
) -> SessionRunClient {
    SessionRunClient::from_runtime(
        session_id,
        ag_runtime::test_support::cli_runtime(backend, kind),
    )
}

impl SessionRunClient {
    /// Injects a scripted adapter for worker tests without exposing it to
    /// workflows.
    pub fn from_channel(session_id: String, channel: Arc<dyn ag_contracts::AgentChannel>) -> Self {
        Self::from_runtime(
            session_id,
            ag_runtime::SessionRuntime::from_channel(channel),
        )
    }
}

impl RuntimeConfig {
    /// Injects a provider transport for worker integration tests.
    pub fn with_app_server(
        client: std::sync::Arc<dyn ag_runtime::test_support::AppServerClient>,
    ) -> Self {
        Self {
            factory: ag_runtime::RuntimeFactory::with_app_server(client),
        }
    }
}
