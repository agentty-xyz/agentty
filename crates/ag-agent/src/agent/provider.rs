//! Shared provider registry and transport policy descriptors.

use std::sync::Arc;

use ag_protocol::{
    AgentResponse, ProtocolRequestProfile, ProtocolSchemaInstructionMode,
    format_protocol_parse_debug_details, parse_protocol_response_strict,
};

use super::backend::{
    AgentBackend, AgentBackendError, AgentPromptTransport, AgentTransport, AppServerThoughtPolicy,
    BuildCommandRequest,
};
use super::prompt;
use super::response_parser::ParsedResponse;
use crate::app_server::AppServerClient;
use crate::model::agent::AgentKind;

/// Factory hook used to build or override provider-specific app-server
/// clients.
type AppServerClientFactory =
    fn(Option<Arc<dyn AppServerClient>>) -> Option<Arc<dyn AppServerClient>>;

/// Creates the backend implementation for the selected agent provider.
pub fn create_backend(kind: AgentKind) -> Box<dyn AgentBackend> {
    (provider_descriptor(kind).backend_factory)()
}

/// Returns the app-server client for the selected provider when applicable.
pub fn create_app_server_client(
    kind: AgentKind,
    default_client: Option<Arc<dyn AppServerClient>>,
) -> Option<Arc<dyn AppServerClient>> {
    (provider_descriptor(kind).app_server_client_factory)(default_client)
}

/// Parses provider output and returns final response content and usage stats.
pub(crate) fn parse_response(kind: AgentKind, stdout: &str, stderr: &str) -> ParsedResponse {
    (provider_descriptor(kind).parse_response)(stdout, stderr)
}

/// Parses one stream line into incremental text and content classification.
///
/// Returns `(text, is_response_content)` where `is_response_content` is `true`
/// for model-authored content and `false` for progress updates.
pub(crate) fn parse_stream_output_line(
    kind: AgentKind,
    stdout_line: &str,
) -> Option<(String, bool)> {
    (provider_descriptor(kind).parse_stream_output_line)(stdout_line)
}

/// Returns transport mode for the selected provider.
pub fn transport_mode(kind: AgentKind) -> AgentTransport {
    provider_descriptor(kind).transport
}

/// Returns whether the provider expects prompts through stdin.
pub(crate) fn prompt_transport(kind: AgentKind) -> AgentPromptTransport {
    provider_descriptor(kind).prompt_transport
}

/// Returns whether bootstrap prompts should include schema text for the
/// selected provider.
///
/// Providers that enforce Agentty's response shape natively still receive
/// policy and field-routing instructions, but skip the large prompt-side JSON
/// Schema to avoid redundant tokens.
pub(crate) fn protocol_schema_instruction_mode(kind: AgentKind) -> ProtocolSchemaInstructionMode {
    provider_descriptor(kind).protocol_schema_instruction_mode
}

/// Parses one final assistant payload strictly against the shared protocol and
/// normalizes it for the active request profile.
///
/// # Errors
/// Returns a descriptive error when provider output does not match the
/// required protocol JSON. The error carries the parse reason and derived
/// diagnostics only: turn errors are rendered into the session transcript, so
/// quoting the payload would print raw provider output into the chat.
pub(crate) fn parse_turn_response(
    kind: AgentKind,
    response_text: &str,
    protocol_profile: ProtocolRequestProfile,
) -> Result<AgentResponse, String> {
    parse_protocol_response_strict(response_text, protocol_profile).map_err(|error| {
        format!(
            "Agent output did not match the required JSON schema from {kind}: \
             {error}\nprotocol_profile: {protocol_profile:?}\ndebug_details:\n{}",
            format_protocol_parse_debug_details(response_text)
        )
    })
}

/// Returns whether one app-server assistant chunk should be treated as
/// thought text instead of transcript output.
pub(crate) fn is_app_server_thought_chunk(
    kind: AgentKind,
    is_delta: bool,
    phase: Option<&str>,
) -> bool {
    if !is_delta {
        return false;
    }

    match provider_descriptor(kind).app_server_thought_policy {
        AppServerThoughtPolicy::None => false,
        AppServerThoughtPolicy::PhaseLabel => phase.is_some_and(is_codex_thought_phase_label),
    }
}

/// Builds one optional stdin payload for providers that stream prompts instead
/// of sending them through argv.
///
/// # Errors
/// Returns an error when provider-specific prompt rendering fails.
pub(crate) fn build_command_stdin_payload(
    kind: AgentKind,
    request: BuildCommandRequest<'_>,
) -> Result<Option<Vec<u8>>, AgentBackendError> {
    let protocol_schema_instruction_mode = protocol_schema_instruction_mode(kind);

    match prompt_transport(kind) {
        AgentPromptTransport::Argv => Ok(None),
        AgentPromptTransport::Stdin => {
            prompt::build_prompt_stdin_payload(request, protocol_schema_instruction_mode, "Claude")
                .map(Some)
        }
    }
}

/// One backend/provider descriptor containing construction and parsing hooks.
struct AgentProviderDescriptor {
    app_server_client_factory: AppServerClientFactory,
    app_server_thought_policy: AppServerThoughtPolicy,
    backend_factory: fn() -> Box<dyn AgentBackend>,
    parse_response: fn(&str, &str) -> ParsedResponse,
    parse_stream_output_line: fn(&str) -> Option<(String, bool)>,
    prompt_transport: AgentPromptTransport,
    protocol_schema_instruction_mode: ProtocolSchemaInstructionMode,
    transport: AgentTransport,
}

fn provider_descriptor(kind: AgentKind) -> AgentProviderDescriptor {
    match kind {
        AgentKind::Antigravity => AgentProviderDescriptor {
            app_server_client_factory: |default_client| {
                Some(default_client.unwrap_or_else(|| {
                    Arc::new(super::app_server::RealAntigravityClient::new())
                        as Arc<dyn AppServerClient>
                }))
            },
            app_server_thought_policy: AppServerThoughtPolicy::None,
            backend_factory: || Box::new(super::antigravity::AntigravityBackend::new()),
            parse_response: super::response_parser::parse_antigravity_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_antigravity_stream_output_line,
            prompt_transport: AgentPromptTransport::Argv,
            protocol_schema_instruction_mode: ProtocolSchemaInstructionMode::TransportSchema,
            transport: AgentTransport::AppServer,
        },
        AgentKind::Gemini => AgentProviderDescriptor {
            app_server_client_factory: |default_client| {
                Some(default_client.unwrap_or_else(|| {
                    Arc::new(super::app_server::RealGeminiAcpClient::new())
                        as Arc<dyn AppServerClient>
                }))
            },
            app_server_thought_policy: AppServerThoughtPolicy::None,
            backend_factory: || Box::new(super::gemini::GeminiBackend),
            parse_response: super::response_parser::parse_gemini_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_gemini_stream_output_line,
            prompt_transport: AgentPromptTransport::Argv,
            protocol_schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
            transport: AgentTransport::AppServer,
        },
        AgentKind::Claude => AgentProviderDescriptor {
            app_server_client_factory: |_default_client| None,
            app_server_thought_policy: AppServerThoughtPolicy::None,
            backend_factory: || Box::new(super::claude::ClaudeBackend),
            parse_response: super::response_parser::parse_claude_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_claude_stream_output_line,
            prompt_transport: AgentPromptTransport::Stdin,
            protocol_schema_instruction_mode: ProtocolSchemaInstructionMode::TransportSchema,
            transport: AgentTransport::Cli,
        },
        AgentKind::Codex => AgentProviderDescriptor {
            app_server_client_factory: |default_client| {
                Some(default_client.unwrap_or_else(|| {
                    Arc::new(super::app_server::RealCodexAppServerClient::new())
                        as Arc<dyn AppServerClient>
                }))
            },
            app_server_thought_policy: AppServerThoughtPolicy::PhaseLabel,
            backend_factory: || Box::new(super::codex::CodexBackend),
            parse_response: super::response_parser::parse_codex_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_codex_stream_output_line,
            prompt_transport: AgentPromptTransport::Argv,
            protocol_schema_instruction_mode: ProtocolSchemaInstructionMode::TransportSchema,
            transport: AgentTransport::AppServer,
        },
    }
}

/// Returns whether one Codex phase label denotes thought/planning text.
///
/// Phase matching is case-insensitive so provider variants such as `Thinking`
/// and `PLAN` continue to route to thought deltas.
fn is_codex_thought_phase_label(phase: &str) -> bool {
    let normalized_phase = phase.trim();

    normalized_phase.eq_ignore_ascii_case("thinking")
        || normalized_phase.eq_ignore_ascii_case("plan")
        || normalized_phase.eq_ignore_ascii_case("reasoning")
        || normalized_phase.eq_ignore_ascii_case("thought")
}

#[cfg(test)]
#[path = "provider_test.rs"]
mod tests;
