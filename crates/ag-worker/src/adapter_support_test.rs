//! Explicit adapter fixtures for offline worker integration tests.
use std::path::PathBuf;
use std::sync::Arc;

use ag_contracts::OneShotError;
pub use ag_runtime::test_support::{
    AgentBackend, AgentBackendError, AppServerClient, AppServerError, AppServerTurnResponse,
    MockAgentBackend, MockAppServerClient, RealOneShotClient, instruction_bootstrap_key,
};
use ag_session::{AgentKind, GraphNode};
use async_trait::async_trait;

use crate::{GraphDispatch, GraphEnvLocation, GraphEnvResolver, RuntimeConfig, SessionRunClient};

/// Env resolver locating every node in one folder, as a host whose nodes all
/// run in the session worktree does. Preparation always succeeds.
pub struct FolderEnvs(PathBuf);

impl FolderEnvs {
    /// Locates every node in `folder`.
    pub fn resolver(folder: impl Into<PathBuf>) -> Arc<dyn GraphEnvResolver> {
        Arc::new(Self(folder.into()))
    }
}

#[async_trait]
impl GraphEnvResolver for FolderEnvs {
    async fn locate(
        &self,
        _graph_id: &str,
        _node: &GraphNode,
    ) -> Result<Option<GraphEnvLocation>, OneShotError> {
        Ok(Some(GraphEnvLocation::Folder(self.0.clone())))
    }

    async fn prepare(
        &self,
        _dispatch: &GraphDispatch,
        _location: &GraphEnvLocation,
    ) -> Result<(), OneShotError> {
        Ok(())
    }
}

/// Creates a worker client backed by a scripted CLI transport.
pub fn cli_session(
    session_id: String,
    backend: Arc<dyn AgentBackend>,
    kind: AgentKind,
) -> SessionRunClient {
    SessionRunClient::from_runtime(
        session_id,
        ag_runtime::test_support::cli_runtime(backend, kind),
        crate::RuntimeConfig::default().execution_policy(kind),
        crate::RuntimeConfig::default().session_admission,
    )
}

impl SessionRunClient {
    /// Injects a scripted adapter for worker tests without exposing it to
    /// workflows.
    pub fn from_channel(session_id: String, channel: Arc<dyn ag_contracts::AgentChannel>) -> Self {
        Self::from_runtime(
            session_id,
            ag_runtime::SessionRuntime::from_channel(channel),
            ag_contracts::ExecutionPolicy::default(),
            crate::RuntimeConfig::default().session_admission,
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
            ..Self::default()
        }
    }

    /// Enables in-process harness work whose provider credentials and `bash`
    /// grants come from a fixed `environment`, so tests can point the
    /// production harness path at a scripted provider.
    #[must_use]
    pub fn with_native_harness_environment(
        mut self,
        data_root: std::path::PathBuf,
        environment: std::collections::BTreeMap<String, String>,
    ) -> Self {
        self.factory = self
            .factory
            .with_native_harness_environment(data_root, environment);

        self
    }
}

/// Serializes unit tests that replace the process-wide telemetry provider.
#[cfg(test)]
pub(crate) static TRACER_PROVIDER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
