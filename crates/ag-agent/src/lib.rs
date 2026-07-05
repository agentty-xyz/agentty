//! Agent backend transports and provider-neutral contracts.
//!
//! This crate owns the agent execution boundary used by Agentty: provider
//! model metadata, turn prompt payloads, CLI/app-server transports, and
//! channel contracts. It intentionally avoids depending on the `agentty`
//! application crate so provider-specific dependencies compile in a leaf
//! workspace member.

pub mod agent;
pub mod app_server;
pub mod app_server_router;
pub mod app_server_transport;
pub mod channel;
pub mod model;

pub use model::agent::{
    AgentCliInfo, AgentCliVersion, AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata,
    ReasoningLevel, parse_persisted_session_agent_model,
};
pub use model::permission::PermissionMode;
pub use model::session::SessionStats;
pub use model::turn_prompt::{
    TurnPrompt, TurnPromptAttachment, TurnPromptContentPart, TurnPromptTextSource,
};
