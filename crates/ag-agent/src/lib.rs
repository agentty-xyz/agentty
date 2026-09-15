//! External-agent adapters implementing the `ag-runtime` execution contracts.
//!
//! This crate owns provider discovery, prompt translation, CLI/app-server
//! transports, retries, and resource cleanup. Shared execution contracts are
//! re-exported from `ag-runtime`; the built-in selection catalog lives in
//! `ag-session`. Hosts inject [`AgentChannel`] and [`OneShotClient`] without
//! depending on the concrete transports.

mod agent;
mod app_server;
pub(crate) mod app_server_transport;
mod channel;
mod input_size;
mod model;
mod provider_call_budget;

pub use agent::{
    AgentAvailabilityProbe, AgentBackend, AgentBackendError, AgentTransport, BuildCommandRequest,
    OneShotClient, OneShotError, OneShotRequest, OneShotSubmission, RealAgentAvailabilityProbe,
    RealOneShotClient, StaticAgentAvailabilityProbe, cleanup_session_worktree_artifacts,
    create_app_server_client, create_backend, diff_fence, executable_name,
    normalize_instruction_conversation_id, transport_mode,
};
#[cfg(any(test, feature = "test-utils"))]
pub use agent::{MockAgentBackend, MockOneShotClient};
#[cfg(any(test, feature = "test-utils"))]
pub use app_server::MockAppServerClient;
pub use app_server::{
    AppServerClient, AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    AppServerTurnResponse,
};
pub use channel::{
    AgentChannel, AgentError, AgentFuture, AgentRequestKind, LiveTranscript, PersonalityPrompt,
    SessionRef, StartSessionRequest, TurnContinuation, TurnEvent, TurnRequest, TurnResult,
    create_agent_channel,
};
#[cfg(any(test, feature = "test-utils"))]
pub use channel::{MockAgentChannel, create_cli_agent_channel_with_backend};
pub use input_size::is_input_size_error;
pub use model::agent::{
    AgentCliInfo, AgentCliVersion, AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata,
    ReasoningLevel, parse_persisted_session_agent_model, resolve_agent_kind_for_model,
    resolve_agent_selection_for_model, resolve_model_for_available_agent_kinds,
    resolve_prompt_model_agent_kind, selectable_models_for_agent_kinds,
};
pub use model::permission::PermissionMode;
pub use model::session::{ResponseStyle, SessionDiffState, SessionStats, SpeedMode};
pub use provider_call_budget::ProviderCallBudget;
