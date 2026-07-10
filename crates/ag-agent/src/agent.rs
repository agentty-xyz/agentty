//! Agent backend wiring split into provider-specific submodules.
//!
//! This module keeps the public API stable while delegating command building
//! and response parsing to focused files under `infra/agent/` using the
//! standard `agent.rs` + `agent/` module layout.

mod antigravity;
pub(crate) mod app_server;
mod availability;
mod backend;
mod claude;
pub(crate) mod cli;
mod codex;
mod gemini;
mod instruction;
mod prompt;
mod provider;
mod response_parser;
mod submission;

pub use availability::{
    AgentAvailabilityProbe, RealAgentAvailabilityProbe, StaticAgentAvailabilityProbe,
    executable_name,
};
pub use backend::{AgentBackend, AgentBackendError, AgentTransport, BuildCommandRequest};
pub use instruction::{
    InstructionDeliveryMode, normalize_instruction_conversation_id,
    plan_app_server_instruction_delivery,
};
pub use prompt::{PromptPreparationRequest, diff_fence, prepare_prompt_text};
pub use provider::{
    build_command_stdin_payload, create_app_server_client, create_backend,
    is_app_server_thought_chunk, parse_response, parse_stream_output_line, parse_turn_response,
    protocol_schema_instruction_mode, provider_kind_for_model, transport_mode,
};
pub use response_parser::{
    ParsedResponse, compact_codex_progress_message, is_codex_completion_status_message,
};
pub use submission::{
    OneShotRequest, OneShotSubmission, submit_one_shot, submit_one_shot_with_app_server_client,
    submit_one_shot_with_backend,
};
#[cfg(any(test, feature = "test-utils"))]
pub mod tests {
    //! Test-only exports for agent backend mocks.

    pub use super::backend::MockAgentBackend;
}
