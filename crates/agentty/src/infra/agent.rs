//! Agent backend wiring split into provider-specific submodules.
//!
//! This module keeps the public API stable while delegating command building
//! and response parsing to focused files under `infra/agent/` using the
//! standard `agent.rs` + `agent/` module layout.

mod backend;
mod claude;
mod codex;
mod gemini;
pub(crate) mod protocol;
mod response_parser;

pub use backend::{
    AgentBackend, AgentBackendError, AgentCommandMode, AgentTransport, BuildCommandRequest,
    create_backend, parse_response, transport_mode,
};
pub(crate) use backend::{
    build_resume_prompt, parse_stream_output_line, prepend_protocol_instructions,
    prepend_repo_root_path_instructions,
};
pub(crate) use protocol::AgentResponse;
pub use response_parser::ParsedResponse;
pub(crate) use response_parser::{
    compact_codex_progress_message, is_codex_completion_status_message,
};

#[cfg(test)]
pub(crate) mod tests {
    //! Test-only exports for agent backend mocks.

    pub(crate) use super::backend::MockAgentBackend;
}
