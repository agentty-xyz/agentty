use std::path::PathBuf;

use ag_contracts::{AgentRequestKind, ReasoningLevel};
use ag_protocol::ProtocolSchemaInstructionMode;

use super::support::{live_transcript, session_resume_request_kind, session_start_request_kind};
use crate::agent::InstructionDeliveryMode;
use crate::app_server::contract::AppServerTurnRequest;
use crate::app_server::prompt::{read_latest_replay_transcript, turn_prompt_for_runtime};

#[test]
fn turn_prompt_for_runtime_adds_repo_root_path_instructions_without_context_reset() {
    // Arrange
    let prompt = "Implement feature";

    // Act
    let turn_prompt = turn_prompt_for_runtime(
        prompt,
        &session_start_request_kind(),
        Some("prior context"),
        InstructionDeliveryMode::BootstrapFull,
        &ag_contracts::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        std::path::Path::new("/tmp/agentty-wt/session-1"),
    )
    .expect("turn prompt should render");

    // Assert
    assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
    assert!(!turn_prompt.contains("summary"));
    assert!(turn_prompt.ends_with(prompt));
}

#[test]
fn turn_prompt_for_runtime_replays_session_output_after_context_reset_with_path_instructions() {
    // Arrange
    let prompt = "Implement feature";

    // Act
    let turn_prompt = turn_prompt_for_runtime(
        prompt,
        &session_resume_request_kind(),
        Some("assistant: proposed plan"),
        InstructionDeliveryMode::BootstrapWithReplay,
        &ag_contracts::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        std::path::Path::new("/tmp/agentty-wt/session-1"),
    )
    .expect("turn prompt should render");

    // Assert
    assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
    assert!(turn_prompt.contains("Continue from the supplied session context"));
    assert!(turn_prompt.contains(r#""assistant: proposed plan""#));
    assert!(turn_prompt.contains("User prompt:\n\nImplement feature"));
}

#[test]
fn turn_prompt_for_runtime_uses_shared_protocol_wrapper_for_utility_prompts() {
    // Arrange
    let prompt = "Generate title";

    // Act
    let turn_prompt = turn_prompt_for_runtime(
        prompt,
        &AgentRequestKind::UtilityPrompt,
        None,
        InstructionDeliveryMode::BootstrapFull,
        &ag_contracts::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        std::path::Path::new("/tmp/agentty-wt/session-1"),
    )
    .expect("turn prompt should render");

    // Assert
    assert!(!turn_prompt.contains("summary"));
    assert!(turn_prompt.ends_with(prompt));
}

#[test]
fn read_latest_replay_transcript_prefers_live_buffer_over_snapshot() {
    // Arrange
    let request = AppServerTurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        provider_call_budget: None,
        folder: PathBuf::from("/tmp"),
        live_transcript: Some(live_transcript("live content from stream")),
        main_checkout_root: None,
        model: "model-a".to_string(),
        permission_mode: ag_contracts::PermissionMode::AutoEdit,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: "Do work".into(),
        request_kind: session_resume_request_kind(),
        replay_transcript: Some("queued snapshot".to_string()),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "session-1".to_string(),
        speed_mode: ag_contracts::SpeedMode::default(),
    };

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, Some("live content from stream".to_string()));
}

#[test]
fn read_latest_replay_transcript_falls_back_to_snapshot_when_live_buffer_is_empty() {
    // Arrange
    let request = AppServerTurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        provider_call_budget: None,
        folder: PathBuf::from("/tmp"),
        live_transcript: Some(live_transcript("")),
        main_checkout_root: None,
        model: "model-a".to_string(),
        permission_mode: ag_contracts::PermissionMode::AutoEdit,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: "Do work".into(),
        request_kind: session_resume_request_kind(),
        replay_transcript: Some("queued snapshot".to_string()),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "session-1".to_string(),
        speed_mode: ag_contracts::SpeedMode::default(),
    };

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, Some("queued snapshot".to_string()));
}

#[test]
fn read_latest_replay_transcript_falls_back_to_snapshot_when_no_live_buffer() {
    // Arrange
    let request = AppServerTurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        provider_call_budget: None,
        folder: PathBuf::from("/tmp"),
        live_transcript: None,
        main_checkout_root: None,
        model: "model-a".to_string(),
        permission_mode: ag_contracts::PermissionMode::AutoEdit,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: "Do work".into(),
        request_kind: session_resume_request_kind(),
        replay_transcript: Some("queued snapshot".to_string()),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "session-1".to_string(),
        speed_mode: ag_contracts::SpeedMode::default(),
    };

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, Some("queued snapshot".to_string()));
}

#[test]
fn read_latest_replay_transcript_returns_none_when_both_are_absent() {
    // Arrange
    let request = AppServerTurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        provider_call_budget: None,
        folder: PathBuf::from("/tmp"),
        live_transcript: None,
        main_checkout_root: None,
        model: "model-a".to_string(),
        permission_mode: ag_contracts::PermissionMode::AutoEdit,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: "Do work".into(),
        request_kind: session_start_request_kind(),
        replay_transcript: None,
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        session_id: "session-1".to_string(),
        speed_mode: ag_contracts::SpeedMode::default(),
    };

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, None);
}
