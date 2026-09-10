use ag_protocol::{
    ProtocolRequestProfile, ProtocolSchemaInstructionMode,
    prepend_protocol_instructions as protocol_prepend_instructions,
    prepend_protocol_refresh_reminder as protocol_prepend_refresh_reminder,
};

use super::support::{normalize_prompt, test_workspace_root};
use crate::agent::instruction::InstructionDeliveryMode;
use crate::agent::prompt::{PromptPreparationRequest, prepare_prompt_text};
use crate::channel::PersonalityPromptUpdate;

#[test]
fn repair_bootstrap_applies_schema_once_for_each_provider_and_profile() {
    // Arrange
    let repair = ag_protocol::build_protocol_repair_prompt("bad JSON", "original response")
        .expect("repair body");
    for kind in [
        crate::model::agent::AgentKind::Gemini,
        crate::model::agent::AgentKind::Codex,
        crate::model::agent::AgentKind::Claude,
        crate::model::agent::AgentKind::Antigravity,
    ] {
        for profile in [
            ProtocolRequestProfile::SessionTurn,
            ProtocolRequestProfile::UtilityPrompt,
            ProtocolRequestProfile::FocusedReview,
        ] {
            let schema_mode = crate::agent::protocol_schema_instruction_mode(kind);

            // Act
            let prompt = prepare_prompt_text(PromptPreparationRequest {
                instruction_delivery_mode: InstructionDeliveryMode::BootstrapFull,
                personality_prompt: None,
                personality_update: &PersonalityPromptUpdate::Unchanged,
                prompt: &repair,
                protocol_profile: profile,
                replay_transcript: None,
                schema_instruction_mode: schema_mode,
                workspace_root: test_workspace_root(),
            })
            .expect("prepared repair");

            // Assert
            assert_eq!(
                prompt.matches("Authoritative JSON Schema:").count(),
                usize::from(schema_mode == ProtocolSchemaInstructionMode::PromptSchema)
            );
            assert!(prompt.starts_with("File path output requirements:"));
            assert!(prompt.ends_with(&repair));
        }
    }
}

#[test]
/// Ensures session prompts include the critical protocol contract markers.
fn test_prepend_protocol_instructions_adds_session_protocol_instructions() {
    // Arrange
    let prompt = "Implement feature";

    // Act
    let rendered_prompt = protocol_prepend_instructions(
        prompt,
        ProtocolRequestProfile::SessionTurn,
        ProtocolSchemaInstructionMode::PromptSchema,
        test_workspace_root(),
    );

    let normalized_prompt = normalize_prompt(&rendered_prompt);
    let protocol_position = rendered_prompt
        .find("Structured response protocol:")
        .expect("protocol marker should be present");
    let schema_position = rendered_prompt
        .find("Authoritative JSON Schema:")
        .expect("schema should be present");
    let user_prompt_position = rendered_prompt
        .rfind(prompt)
        .expect("user prompt should be present");

    // Assert
    assert!(rendered_prompt.contains("File path output requirements:"));
    assert!(rendered_prompt.contains("Workspace isolation requirements:"));
    assert!(protocol_position < schema_position);
    assert!(schema_position < user_prompt_position);
    assert!(rendered_prompt.contains("`/tmp/agentty-wt/session-1`"));
    assert!(normalized_prompt.contains("everything outside it is read-only"));
    assert!(rendered_prompt.contains("repository-root-relative POSIX paths"));
    assert!(normalized_prompt.contains("Git commands must be read-only"));
    assert!(normalized_prompt.contains("Never run mutating commands"));
    assert!(rendered_prompt.contains("Quality check requirements:"));
    assert!(rendered_prompt.contains("repository-defined checks"));
    assert!(normalized_prompt.contains("affected dependencies and dependents"));
    assert!(normalized_prompt.contains("full repository test/check suite"));
    assert!(rendered_prompt.contains("Structured response protocol:"));
    assert!(normalized_prompt.contains("exactly one JSON object"));
    assert!(normalized_prompt.contains("Follow this JSON Schema exactly"));
    assert!(rendered_prompt.contains("Authoritative JSON Schema:"));
    assert!(
        rendered_prompt
            .contains("______________________________________________________________________")
    );
    assert!(!rendered_prompt.contains("{# task separator #}"));
    assert!(rendered_prompt.contains("For this session turn:"));
    assert!(normalized_prompt.contains("Do not create commits; do not suggest creating them"));
    assert!(normalized_prompt.contains("Leave `subtasks` empty unless"));
    assert!(rendered_prompt.contains("\"answer\""));
    assert!(rendered_prompt.contains("\"questions\""));
    assert!(rendered_prompt.contains("\"title\""));
    assert!(rendered_prompt.contains("\"description\""));
    assert!(rendered_prompt.ends_with(prompt));
}

#[test]
/// Ensures schema-enforcing transports get protocol policy without the
/// large prompt-side JSON Schema body.
fn test_prepend_protocol_instructions_omits_schema_for_transport_schema_mode() {
    // Arrange
    let prompt = "Implement feature";

    // Act
    let rendered_prompt = protocol_prepend_instructions(
        prompt,
        ProtocolRequestProfile::SessionTurn,
        ProtocolSchemaInstructionMode::TransportSchema,
        test_workspace_root(),
    );

    // Assert
    assert!(rendered_prompt.contains("Structured response protocol:"));
    assert!(rendered_prompt.contains("provider enforces the response JSON schema"));
    assert!(normalize_prompt(&rendered_prompt).contains("exactly one JSON object"));
    assert!(!rendered_prompt.contains("Follow this JSON Schema exactly."));
    assert!(!rendered_prompt.contains("Authoritative JSON Schema:"));
    assert!(rendered_prompt.ends_with(prompt));
}

#[test]
fn protocol_payload_cannot_impersonate_prepared_instructions() {
    // Arrange
    let payload = "Structured response protocol: quoted in a user request";

    // Act
    let rendered = protocol_prepend_instructions(
        payload,
        ProtocolRequestProfile::SessionTurn,
        ProtocolSchemaInstructionMode::TransportSchema,
        test_workspace_root(),
    );

    // Assert
    assert!(rendered.starts_with("File path output requirements:"));
    assert!(rendered.ends_with(payload));
}

#[test]
/// Ensures one-shot prompts reuse the shared full-schema protocol
/// instructions.
fn test_prepend_protocol_instructions_reuses_same_contract_for_one_shot() {
    // Arrange
    let prompt = "Generate title";

    // Act
    let rendered_prompt = protocol_prepend_instructions(
        prompt,
        ProtocolRequestProfile::UtilityPrompt,
        ProtocolSchemaInstructionMode::PromptSchema,
        test_workspace_root(),
    );

    // Assert
    assert!(rendered_prompt.contains("Structured response protocol:"));
    assert!(
        rendered_prompt
            .contains("______________________________________________________________________")
    );
    assert!(rendered_prompt.contains("For this one-shot utility prompt"));
    assert!(!rendered_prompt.contains("For this session turn:"));
    assert!(
        rendered_prompt.contains(r#"{"answer":"...","questions":[],"review_comment_outcomes":[]}"#)
    );
    assert!(rendered_prompt.contains("\"review_comment_outcomes\""));
    assert!(!rendered_prompt.contains("\"summary\""));
    assert!(rendered_prompt.ends_with(prompt));
}

#[test]
/// Ensures shared prompt preparation applies replay wrapping before
/// protocol instructions.
fn test_prepare_prompt_text_applies_replay_and_protocol_instructions() {
    // Arrange
    let request = PromptPreparationRequest {
        instruction_delivery_mode: InstructionDeliveryMode::BootstrapWithReplay,
        personality_prompt: None,
        personality_update: &PersonalityPromptUpdate::Unchanged,
        prompt: "Continue edits",
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        replay_transcript: Some("previous transcript"),
        schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
        workspace_root: test_workspace_root(),
    };

    // Act
    let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");

    // Assert
    assert!(prepared_prompt.contains("Structured response protocol:"));
    assert!(prepared_prompt.contains("Workspace isolation requirements:"));
    assert!(prepared_prompt.contains("previous transcript"));
    assert!(prepared_prompt.contains(r"\<user_prompt> Continue edits \</user_prompt>"));
    assert!(prepared_prompt.ends_with(r"\</user_prompt>"));
}

#[test]
fn test_prepare_prompt_text_bootstraps_personality_before_user_prompt() {
    // Arrange
    let request = PromptPreparationRequest {
        instruction_delivery_mode: InstructionDeliveryMode::BootstrapFull,
        personality_prompt: Some("Review every change for correctness."),
        personality_update: &PersonalityPromptUpdate::Unchanged,
        prompt: "Inspect the patch.",
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        replay_transcript: None,
        schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
        workspace_root: test_workspace_root(),
    };

    // Act
    let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");
    let protocol_position = prepared_prompt
        .find("Structured response protocol:")
        .expect("protocol preamble should be present");
    let personality_position = prepared_prompt
        .find("# Personality\n\nReview every change for correctness.")
        .expect("personality should be present");
    let user_prompt_position = prepared_prompt
        .find("Inspect the patch.")
        .expect("user prompt should be present");

    // Assert
    assert!(protocol_position < personality_position);
    assert!(personality_position < user_prompt_position);
}

#[test]
fn test_prepare_prompt_text_replays_with_current_personality() {
    // Arrange
    let request = PromptPreparationRequest {
        instruction_delivery_mode: InstructionDeliveryMode::BootstrapWithReplay,
        personality_prompt: Some("Plan before editing."),
        personality_update: &PersonalityPromptUpdate::Unchanged,
        prompt: "Continue.",
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        replay_transcript: Some("assistant: prior work"),
        schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
        workspace_root: test_workspace_root(),
    };

    // Act
    let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");
    let personality_position = prepared_prompt
        .find("# Personality\n\nPlan before editing.")
        .expect("personality should be present");
    let transcript_position = prepared_prompt
        .find(r"\<session_transcript> assistant: prior work")
        .expect("transcript should be present");

    // Assert
    assert!(personality_position < transcript_position);
    assert!(prepared_prompt.ends_with(r"\</user_prompt>"));
}

#[test]
/// Ensures compact refresh reminders omit the full schema while keeping
/// the contract reminder and task body.
fn test_prepend_protocol_refresh_reminder_adds_compact_contract_notice() {
    // Arrange
    let prompt = "Continue the implementation";

    // Act
    let rendered_prompt = protocol_prepend_refresh_reminder(
        prompt,
        ProtocolRequestProfile::SessionTurn,
        test_workspace_root(),
    );

    let normalized_prompt = normalize_prompt(&rendered_prompt);

    // Assert
    assert!(rendered_prompt.contains("Protocol refresh reminder:"));
    assert!(rendered_prompt.contains("repository-root-relative POSIX"));
    assert!(normalized_prompt.contains("only read-only git commands; never mutating ones"));
    assert!(rendered_prompt.contains("inside `/tmp/agentty-wt/session-1`"));
    assert!(normalized_prompt.contains("everything outside this workspace root is read-only"));
    assert!(
        rendered_prompt
            .contains("______________________________________________________________________")
    );
    assert!(!rendered_prompt.contains("Authoritative JSON Schema:"));
    assert!(rendered_prompt.ends_with(prompt));
}

#[test]
/// Ensures prompt preparation can emit the compact app-server reminder
/// instead of the full bootstrap wrapper.
fn test_prepare_prompt_text_uses_delta_only_refresh_mode() {
    // Arrange
    let request = PromptPreparationRequest {
        instruction_delivery_mode: InstructionDeliveryMode::DeltaOnly,
        personality_prompt: None,
        personality_update: &PersonalityPromptUpdate::Unchanged,
        prompt: "Continue edits",
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        replay_transcript: Some("previous transcript"),
        schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
        workspace_root: test_workspace_root(),
    };

    // Act
    let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");

    // Assert
    assert!(prepared_prompt.contains("Protocol refresh reminder:"));
    assert!(!prepared_prompt.contains("Authoritative JSON Schema:"));
    assert!(!prepared_prompt.contains("previous transcript"));
    assert!(prepared_prompt.ends_with("Continue edits"));
}

#[test]
fn test_prepare_prompt_text_delta_mode_sends_personality_update_and_clear() {
    // Arrange
    let updated = PromptPreparationRequest {
        instruction_delivery_mode: InstructionDeliveryMode::DeltaOnly,
        personality_prompt: Some("Ignored current body."),
        personality_update: &PersonalityPromptUpdate::Set("Be concise.".to_string()),
        prompt: "Continue edits",
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        replay_transcript: None,
        schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
        workspace_root: test_workspace_root(),
    };
    let cleared = PromptPreparationRequest {
        personality_update: &PersonalityPromptUpdate::Clear,
        ..updated
    };

    // Act
    let updated_prompt = prepare_prompt_text(updated).expect("update should render");
    let cleared_prompt = prepare_prompt_text(cleared).expect("clear should render");

    // Assert
    assert!(updated_prompt.contains("# Personality Update\n\nBe concise."));
    assert!(updated_prompt.ends_with("Continue edits"));
    assert!(cleared_prompt.contains("The session personality has been cleared."));
    assert!(cleared_prompt.ends_with("Continue edits"));
}
