//! Shared CLI channel fixture exposed through `test-utils`.

use std::sync::Arc;

use ag_contracts::AgentChannel;
use ag_session::AgentKind;

use crate::agent;
use crate::channel::cli::CliAgentChannel;

/// Creates a CLI channel backed by an injected backend for tests.
pub fn create_cli_agent_channel_with_backend(
    backend: Arc<dyn agent::AgentBackend>,
    kind: AgentKind,
) -> Arc<dyn AgentChannel> {
    Arc::new(CliAgentChannel::with_backend(backend, kind))
}
