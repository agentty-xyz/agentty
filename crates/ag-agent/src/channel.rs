//! Provider-agnostic agent channel module.
//!
//! This parent module intentionally acts as a router only: child modules hold
//! concrete transport adapters while this module re-exports the channel API
//! consumed from the crate root.

pub(crate) mod app_server;
pub(crate) mod cli;
mod contract;
mod factory;

#[cfg(any(test, feature = "test-utils"))]
pub use contract::MockAgentChannel;
pub use contract::{
    AgentChannel, AgentError, AgentFuture, AgentRequestKind, LiveTranscript, SessionRef,
    StartSessionRequest, TurnEvent, TurnRequest, TurnResult,
};
pub use factory::create_agent_channel;
#[cfg(any(test, feature = "test-utils"))]
pub use factory::create_cli_agent_channel_with_backend;
