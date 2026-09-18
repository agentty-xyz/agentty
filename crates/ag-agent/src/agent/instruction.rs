//! Provider-managed instruction bootstrap planning for app-server sessions.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::sync::LazyLock;

use ag_contracts::{AgentRequestKind, normalize_instruction_conversation_id};
use ag_protocol::{
    ProtocolRequestProfile, ProtocolSchemaInstructionMode, prepend_protocol_instructions,
    prepend_protocol_refresh_reminder,
};

/// Fingerprints every bootstrap template and task schema once per process.
static INSTRUCTION_FINGERPRINT: LazyLock<u64> = LazyLock::new(|| {
    let mut hasher = DefaultHasher::new();
    for profile in [
        ProtocolRequestProfile::SessionTurn,
        ProtocolRequestProfile::UtilityPrompt,
        ProtocolRequestProfile::FocusedReview,
        ProtocolRequestProfile::ReviewMetadata,
    ] {
        for mode in [
            ProtocolSchemaInstructionMode::PromptSchema,
            ProtocolSchemaInstructionMode::TransportSchema,
        ] {
            prepend_protocol_instructions("", profile, mode, Path::new("WORKSPACE"))
                .hash(&mut hasher);
        }
        prepend_protocol_refresh_reminder("", profile, Path::new("WORKSPACE")).hash(&mut hasher);
    }
    include_str!("template/resume_with_transcript_prompt.md").hash(&mut hasher);
    include_str!("template/personality_prompt.md").hash(&mut hasher);
    include_str!("template/response_style_prompt.md").hash(&mut hasher);
    hasher.finish()
});

/// Returns the opaque persisted bootstrap key for a provider conversation.
///
/// Legacy unversioned conversation IDs intentionally do not match this key:
/// upgrading the application refreshes policy before allowing compact turns.
#[must_use]
pub fn instruction_bootstrap_key(conversation_id: Option<&str>) -> Option<String> {
    normalize_instruction_conversation_id(conversation_id)
        .map(|id| format!("v1:{:016x}:{}:{id}", *INSTRUCTION_FINGERPRINT, id.len()))
}

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
            | AgentRequestKind::ReviewMetadata
            | AgentRequestKind::UtilityPrompt
            | AgentRequestKind::AccountRead
    ) {
        return InstructionDeliveryMode::BootstrapFull;
    }

    let current_id = instruction_bootstrap_key(current_provider_conversation_id);
    if current_id.is_some() && current_id.as_deref() == persisted_instruction_conversation_id {
        return InstructionDeliveryMode::DeltaOnly;
    }

    InstructionDeliveryMode::BootstrapFull
}

#[cfg(test)]
#[path = "instruction_test.rs"]
mod tests;
