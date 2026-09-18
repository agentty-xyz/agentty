//! External-agent adapters implementing the `ag-contracts` execution contracts.
//!
//! This crate owns provider discovery, prompt translation, CLI/app-server
//! transports, retries, and resource cleanup. Shared execution contracts live
//! in `ag-contracts`; the built-in selection catalog lives in
//! `ag-session`. Only `ag-runtime` constructs these adapters; application
//! workflows receive worker clients instead of concrete transports.

mod agent;
mod app_server;
pub(crate) mod app_server_transport;
mod channel;
mod model;

pub(crate) use ag_contracts::is_input_size_error;
#[cfg(any(test, feature = "test-utils"))]
pub use agent::MockAgentBackend;
pub use agent::{
    AgentAvailabilityProbe, AgentBackend, AgentBackendError, AgentTransport, BuildCommandRequest,
    RealAgentAvailabilityProbe, RealOneShotClient, StaticAgentAvailabilityProbe,
    cleanup_session_worktree_artifacts, create_app_server_client, create_backend, executable_name,
    instruction_bootstrap_key, transport_mode,
};
#[cfg(any(test, feature = "test-utils"))]
pub use app_server::MockAppServerClient;
pub use app_server::{
    AppServerClient, AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    AppServerTurnResponse,
};
pub use channel::create_agent_channel;
#[cfg(any(test, feature = "test-utils"))]
pub use channel::create_cli_agent_channel_with_backend;
pub use model::agent::{AgentCliInfo, AgentCliVersion};
