use ag_contracts::AgentRequestKind;

use crate::agent::instruction::{
    InstructionDeliveryMode, instruction_bootstrap_key, plan_app_server_instruction_delivery,
};

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
    let persisted_instruction_conversation_id = instruction_bootstrap_key(Some("thread-123"));

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
    let persisted_instruction_conversation_id = instruction_bootstrap_key(Some("thread-123"));

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
    let persisted_instruction_conversation_id = instruction_bootstrap_key(Some("thread-123"));

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
    let persisted_instruction_conversation_id = instruction_bootstrap_key(Some("thread-123"));

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

#[test]
fn legacy_or_changed_policy_keys_force_bootstrap() {
    // Arrange
    let current = instruction_bootstrap_key(Some("thread-123")).expect("key");
    let stale = current.replace("v1:", "v0:");

    let stale_fingerprint = "v1:0000000000000000:10:thread-123";
    for key in ["thread-123", stale.as_str(), stale_fingerprint] {
        // Act
        let mode = plan_app_server_instruction_delivery(
            &AgentRequestKind::SessionResume,
            Some("thread-123"),
            Some(key),
            false,
        );

        // Assert
        assert_eq!(mode, InstructionDeliveryMode::BootstrapFull);
    }
    assert_eq!(instruction_bootstrap_key(None), None);
    assert_eq!(instruction_bootstrap_key(Some("   ")), None);
    assert_eq!(
        instruction_bootstrap_key(Some(" thread-123 ")),
        Some(current)
    );
}
