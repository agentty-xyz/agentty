use crate::agent::instruction::{
    InstructionDeliveryMode, normalize_instruction_conversation_id,
    plan_app_server_instruction_delivery,
};
use crate::channel::AgentRequestKind;

#[test]
fn missing_conversation_ids_require_full_bootstrap() {
    // Arrange
    let ids = [None, Some(""), Some("   ")];

    for current_id in ids {
        // Act
        let mode = plan_app_server_instruction_delivery(
            &AgentRequestKind::SessionStart,
            current_id,
            None,
            false,
        );

        // Assert
        assert_eq!(mode, InstructionDeliveryMode::BootstrapFull);
    }
}

#[test]
/// Reuses the provider-managed bootstrap only when the persisted state
/// still matches the active provider conversation.
fn test_plan_app_server_instruction_delivery_uses_delta_only_for_matching_state() {
    // Arrange
    let persisted_instruction_conversation_id =
        normalize_instruction_conversation_id(Some("thread-123"));

    // Act
    let mode = plan_app_server_instruction_delivery(
        &AgentRequestKind::SessionResume,
        Some("thread-123"),
        persisted_instruction_conversation_id.as_deref(),
        false,
    );

    // Assert
    assert_eq!(mode, InstructionDeliveryMode::DeltaOnly);
}

#[test]
/// Forces a replay bootstrap whenever the runtime lost provider-managed
/// context for the active turn.
fn test_plan_app_server_instruction_delivery_uses_bootstrap_with_replay_after_reset() {
    // Arrange
    let persisted_instruction_conversation_id =
        normalize_instruction_conversation_id(Some("thread-123"));

    // Act
    let mode = plan_app_server_instruction_delivery(
        &AgentRequestKind::SessionResume,
        Some("thread-456"),
        persisted_instruction_conversation_id.as_deref(),
        true,
    );

    // Assert
    assert_eq!(mode, InstructionDeliveryMode::BootstrapWithReplay);
}

#[test]
/// Requires a fresh bootstrap when the provider conversation changed.
fn test_plan_app_server_instruction_delivery_bootstraps_full_for_new_context() {
    // Arrange
    let persisted_instruction_conversation_id =
        normalize_instruction_conversation_id(Some("thread-123"));

    // Act
    let mode = plan_app_server_instruction_delivery(
        &AgentRequestKind::SessionResume,
        Some("thread-456"),
        persisted_instruction_conversation_id.as_deref(),
        false,
    );

    // Assert
    assert_eq!(mode, InstructionDeliveryMode::BootstrapFull);
}

#[test]
/// Keeps one-shot utility prompts on the full bootstrap path because they
/// do not reuse long-lived provider context.
fn test_plan_app_server_instruction_delivery_bootstraps_full_for_utility_prompt() {
    // Arrange
    let persisted_instruction_conversation_id =
        normalize_instruction_conversation_id(Some("thread-123"));

    // Act
    let mode = plan_app_server_instruction_delivery(
        &AgentRequestKind::UtilityPrompt,
        Some("thread-123"),
        persisted_instruction_conversation_id.as_deref(),
        false,
    );

    // Assert
    assert_eq!(mode, InstructionDeliveryMode::BootstrapFull);
}
