//! Adapter fixtures for testing runtime integration without live providers.
use std::sync::Arc;

pub use ag_agent::{
    AgentBackend, AgentBackendError, AppServerClient, AppServerError, AppServerTurnResponse,
    MockAgentBackend, MockAppServerClient, RealOneShotClient,
};
use ag_contracts::{AgentChannel, OneShotClient};
use ag_session::AgentKind;

use super::{RuntimeFactory, SessionRuntime, UtilityRuntime};

/// Wraps a scripted command backend in the real CLI runtime.
pub fn cli_runtime(backend: std::sync::Arc<dyn AgentBackend>, kind: AgentKind) -> SessionRuntime {
    SessionRuntime::from_channel(ag_agent::create_cli_agent_channel_with_backend(
        backend, kind,
    ))
}

impl RuntimeFactory {
    /// Injects a transport for offline provider integration tests.
    pub fn with_app_server(client: Arc<dyn ag_agent::AppServerClient>) -> Self {
        Self {
            app_server: Some(client),
        }
    }
}

impl SessionRuntime {
    /// Wraps a scripted adapter for worker tests.
    pub fn from_channel(channel: Arc<dyn AgentChannel>) -> Self {
        Self { channel }
    }
}

impl UtilityRuntime {
    /// Wraps an injected utility adapter for deterministic worker tests.
    pub fn from_client(client: Arc<dyn OneShotClient>) -> Self {
        Self { client }
    }
}
