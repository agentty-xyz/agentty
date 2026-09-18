//! Provider-managed instruction bootstrap planning for app-server sessions.

use ag_contracts::{AgentRequestKind, normalize_instruction_conversation_id};

/// Prompt-shaping mode used for one app-server turn attempt.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum InstructionDeliveryMode {
    /// Send the full instruction contract without transcript replay.
    BootstrapFull,
    /// Reuse the existing provider-managed bootstrap and send only a compact
    /// reminder.
    DeltaOnly,
    /// Re-send the full instruction contract while replaying the transcript
    /// after context loss.
    BootstrapWithReplay,
}

/// Plans how one app-server turn should deliver Agentty's instruction
/// contract.
pub(crate) fn plan_app_server_instruction_delivery(
    request_kind: &AgentRequestKind,
    current_provider_conversation_id: Option<&str>,
    persisted_instruction_conversation_id: Option<&str>,
    should_replay_transcript: bool,
) -> InstructionDeliveryMode {
    if should_replay_transcript {
        return InstructionDeliveryMode::BootstrapWithReplay;
    }

    if matches!(
        request_kind,
        AgentRequestKind::FocusedReview
            | AgentRequestKind::UtilityPrompt
            | AgentRequestKind::AccountRead
    ) {
        return InstructionDeliveryMode::BootstrapFull;
    }

    let current_id = normalize_instruction_conversation_id(current_provider_conversation_id);
    if current_id.is_some() && current_id.as_deref() == persisted_instruction_conversation_id {
        return InstructionDeliveryMode::DeltaOnly;
    }

    InstructionDeliveryMode::BootstrapFull
}

#[cfg(test)]
#[path = "instruction_test.rs"]
mod tests;
