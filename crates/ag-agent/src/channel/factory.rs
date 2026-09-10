//! Channel factory for routing providers to transport adapters.

use std::sync::Arc;

use crate::agent;
use crate::app_server::AppServerClient;
use crate::channel::app_server::AppServerAgentChannel;
use crate::channel::cli::CliAgentChannel;
use crate::channel::contract::AgentChannel;
use crate::model::agent::AgentKind;

/// Creates the provider-specific channel for the given agent kind.
///
/// Claude uses [`CliAgentChannel`]; persistent runtime providers
/// (Antigravity, Gemini, Codex) use [`AppServerAgentChannel`].
pub fn create_agent_channel(
    kind: AgentKind,
    app_server_client_override: Option<Arc<dyn AppServerClient>>,
) -> Arc<dyn AgentChannel> {
    let backend = agent::create_backend(kind);
    let transport = agent::transport_mode(kind);

    if transport.uses_app_server() {
        match agent::create_app_server_client(kind, app_server_client_override) {
            Some(app_server_client) => {
                Arc::new(AppServerAgentChannel::new(app_server_client, kind))
            }
            None => Arc::new(CliAgentChannel::with_backend(Arc::from(backend), kind)),
        }
    } else {
        Arc::new(CliAgentChannel::with_backend(Arc::from(backend), kind))
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[path = "factory_support_test.rs"]
mod support;

#[cfg(any(test, feature = "test-utils"))]
pub use support::create_cli_agent_channel_with_backend;

#[cfg(test)]
#[path = "factory_test.rs"]
mod tests;
