//! Shared app-server prompt shaping helpers.

use std::path::Path;

use ag_protocol::{ProtocolSchemaInstructionMode, TurnPrompt, TurnPromptTextSource};

use crate::agent;
use crate::agent::InstructionDeliveryMode;
use crate::app_server::{AppServerError, AppServerTurnRequest};
use crate::channel::AgentRequestKind;

/// Reads the latest replay transcript, preferring the live source over the
/// queued snapshot.
///
/// The live source accumulates all transcript messages in real time,
/// including content from a turn that failed mid-stream. When available, it
/// provides a more complete transcript than the snapshot captured at
/// turn-enqueue time.
pub(crate) fn read_latest_replay_transcript(request: &AppServerTurnRequest) -> Option<String> {
    if let Some(live_transcript) = &request.live_transcript
        && let Some(transcript_text) = live_transcript.replay_text()
        && !transcript_text.trim().is_empty()
    {
        return Some(transcript_text);
    }

    request.replay_transcript.clone()
}

/// Returns the turn prompt, applying protocol preamble and optional context
/// replay according to the selected instruction delivery mode.
///
///
/// `BootstrapFull` and `BootstrapWithReplay` include the shared protocol
/// preamble, while `DeltaOnly` emits only a compact reminder for provider
/// contexts that already received that contract. Providers that enforce the
/// response schema at transport level can request a policy-only bootstrap
/// preamble to avoid duplicating the full JSON Schema in prompt text.
///
/// # Errors
/// Returns an error when Askama prompt rendering fails after a context reset.
pub(crate) fn turn_prompt_for_runtime(
    prompt: impl Into<TurnPrompt>,
    request_kind: &AgentRequestKind,
    replay_transcript: Option<&str>,
    instruction_delivery_mode: InstructionDeliveryMode,
    personality: &crate::channel::PersonalityPrompt,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    workspace_root: &Path,
) -> Result<TurnPrompt, AppServerError> {
    let prompt = prompt.into();
    let agent_prompt = prompt.agent_text();
    let turn_prompt = agent::prepare_prompt_text(agent::PromptPreparationRequest {
        instruction_delivery_mode,
        personality_prompt: personality.current(),
        personality_update: personality.update(),
        prompt: &agent_prompt,
        protocol_profile: request_kind.protocol_profile(),
        replay_transcript,
        schema_instruction_mode,
        workspace_root,
    })
    .map_err(|error| AppServerError::PromptRender(error.to_string()))?;

    Ok(TurnPrompt {
        attachments: prompt.attachments,
        text: turn_prompt,
        text_source: TurnPromptTextSource::AgentData,
    })
}

/// Plans how one app-server turn should deliver Agentty's instruction
/// contract for the active runtime context.
pub(crate) fn instruction_delivery_mode_for_runtime(
    request: &AppServerTurnRequest,
    runtime_provider_conversation_id: Option<&str>,
    should_replay_transcript: bool,
) -> InstructionDeliveryMode {
    agent::plan_app_server_instruction_delivery(
        &request.request_kind,
        runtime_provider_conversation_id,
        request.persisted_instruction_conversation_id.as_deref(),
        should_replay_transcript,
    )
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
