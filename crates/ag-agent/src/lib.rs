//! Agent backend transports and provider-neutral contracts.
//!
//! This crate owns the agent execution boundary used by Agentty: provider
//! model metadata, turn prompt payloads, CLI/app-server transports, and
//! channel contracts. It intentionally avoids depending on the `agentty`
//! application crate so provider-specific dependencies compile in a leaf
//! workspace member.

mod agent;
mod app_server;
pub(crate) mod app_server_transport;
mod channel;
mod model;

#[cfg(any(test, feature = "test-utils"))]
pub use agent::MockAgentBackend;
pub use agent::{
    AgentAvailabilityProbe, AgentBackend, AgentBackendError, AgentTransport, BuildCommandRequest,
    OneShotRequest, OneShotSubmission, RealAgentAvailabilityProbe, StaticAgentAvailabilityProbe,
    cleanup_session_worktree_artifacts, create_app_server_client, create_backend, diff_fence,
    executable_name, normalize_instruction_conversation_id, submit_one_shot,
    submit_one_shot_with_app_server_client, submit_one_shot_with_backend, transport_mode,
};
#[cfg(any(test, feature = "test-utils"))]
pub use app_server::MockAppServerClient;
pub use app_server::{
    AppServerClient, AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    AppServerTurnResponse,
};
pub use channel::{
    AgentChannel, AgentError, AgentFuture, AgentRequestKind, LiveTranscript, SessionRef,
    StartSessionRequest, TurnEvent, TurnRequest, TurnResult, create_agent_channel,
};
#[cfg(any(test, feature = "test-utils"))]
pub use channel::{MockAgentChannel, create_cli_agent_channel_with_backend};
pub use model::agent::{
    AgentCliInfo, AgentCliVersion, AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata,
    ReasoningLevel, parse_persisted_session_agent_model, resolve_agent_kind_for_model,
    resolve_agent_selection_for_model, resolve_model_for_available_agent_kinds,
    resolve_prompt_model_agent_kind, selectable_models_for_agent_kinds,
};
pub use model::permission::PermissionMode;
pub use model::session::SessionStats;
