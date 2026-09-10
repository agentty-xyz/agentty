use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_protocol::{ProtocolSchemaInstructionMode, TurnPrompt};

use crate::agent;
use crate::agent::InstructionDeliveryMode;
use crate::app_server::contract::AppServerTurnRequest;
use crate::app_server::prompt::{
    instruction_delivery_mode_for_runtime, read_latest_replay_transcript, turn_prompt_for_runtime,
};
use crate::channel::{AgentRequestKind, LiveTranscript};
use crate::model::agent::ReasoningLevel;

/// Workspace root used by app-server prompt shaping tests.
const TEST_WORKSPACE_ROOT: &str = "/tmp/agentty-wt/session-1";

#[derive(Debug)]
struct TestLiveTranscript {
    text: String,
}

impl LiveTranscript for TestLiveTranscript {
    fn replay_text(&self) -> Option<String> {
        Some(self.text.clone())
    }
}

fn live_transcript(text: &str) -> Arc<dyn LiveTranscript> {
    Arc::new(TestLiveTranscript {
        text: text.to_string(),
    })
}

/// Returns one persisted bootstrap marker that matches the active
/// app-server instruction contract for session turns.
fn persisted_instruction_conversation_id_for_session_turn(
    provider_conversation_id: Option<&str>,
) -> Option<String> {
    agent::normalize_instruction_conversation_id(provider_conversation_id)
}

#[test]
fn read_latest_replay_transcript_prefers_live_source() {
    // Arrange
    let request = AppServerTurnRequest {
        provider_call_budget: None,
        folder: PathBuf::from("/tmp/test"),
        live_transcript: Some(live_transcript("live content")),
        main_checkout_root: None,
        model: "test-model".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("hello"),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        replay_transcript: None,
        session_id: "test-session".to_string(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, Some("live content".to_string()));
}

#[test]
fn read_latest_replay_transcript_falls_back_when_live_source_is_empty() {
    // Arrange
    let request = AppServerTurnRequest {
        provider_call_budget: None,
        folder: PathBuf::from("/tmp/test"),
        live_transcript: Some(live_transcript("  ")),
        main_checkout_root: None,
        model: "test-model".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("hello"),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        replay_transcript: Some("queued transcript".to_string()),
        session_id: "test-session".to_string(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert_eq!(output, Some("queued transcript".to_string()));
}

#[test]
fn read_latest_replay_transcript_returns_none_when_no_replay_text() {
    // Arrange
    let request = AppServerTurnRequest {
        provider_call_budget: None,
        folder: PathBuf::from("/tmp/test"),
        live_transcript: None,
        main_checkout_root: None,
        model: "test-model".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("hello"),
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        replay_transcript: None,
        session_id: "test-session".to_string(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };

    // Act
    let output = read_latest_replay_transcript(&request);

    // Assert
    assert!(output.is_none());
}

#[test]
fn turn_prompt_for_runtime_includes_protocol_preamble() {
    // Arrange
    let prompt = TurnPrompt::from("fix the bug");
    let request_kind = AgentRequestKind::SessionStart;

    // Act
    let result = turn_prompt_for_runtime(
        prompt,
        &request_kind,
        None,
        InstructionDeliveryMode::BootstrapFull,
        &crate::channel::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        Path::new(TEST_WORKSPACE_ROOT),
    );
    let turn_prompt = result.expect("prompt rendering should succeed");
    let normalized_prompt = turn_prompt
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    // Assert
    assert!(turn_prompt.text.contains("fix the bug"));
    assert!(turn_prompt.text.contains("Structured response protocol:"));
    assert!(normalized_prompt.contains("everything outside it is read-only"));
}

#[test]
fn turn_prompt_for_runtime_omits_full_schema_for_transport_schema_mode() {
    // Arrange
    let prompt = TurnPrompt::from("fix the bug");
    let request_kind = AgentRequestKind::SessionStart;

    // Act
    let result = turn_prompt_for_runtime(
        prompt,
        &request_kind,
        None,
        InstructionDeliveryMode::BootstrapFull,
        &crate::channel::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::TransportSchema,
        Path::new(TEST_WORKSPACE_ROOT),
    );

    // Assert
    let turn_prompt = result.expect("prompt rendering should succeed");
    assert!(turn_prompt.text.contains("Structured response protocol:"));
    assert!(
        turn_prompt
            .text
            .contains("provider enforces the response JSON schema")
    );
    assert!(!turn_prompt.text.contains("Authoritative JSON Schema:"));
}

#[test]
fn turn_prompt_for_runtime_uses_compact_refresh_reminder_for_delta_only() {
    // Arrange
    let prompt = TurnPrompt::from("continue the fix");
    let request_kind = AgentRequestKind::SessionResume;

    // Act
    let result = turn_prompt_for_runtime(
        prompt,
        &request_kind,
        None,
        InstructionDeliveryMode::DeltaOnly,
        &crate::channel::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        Path::new(TEST_WORKSPACE_ROOT),
    );

    // Assert
    let turn_prompt = result.expect("prompt rendering should succeed");
    assert!(turn_prompt.text.contains("Protocol refresh reminder:"));
    assert!(!turn_prompt.text.contains("Authoritative JSON Schema:"));
}

#[test]
fn turn_prompt_for_runtime_rewrites_user_at_lookups_for_agent_delivery() {
    // Arrange
    let prompt = TurnPrompt::from("review @src/main.rs");
    let request_kind = AgentRequestKind::SessionStart;

    // Act
    let result = turn_prompt_for_runtime(
        prompt,
        &request_kind,
        None,
        InstructionDeliveryMode::BootstrapFull,
        &crate::channel::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        Path::new(TEST_WORKSPACE_ROOT),
    );

    // Assert
    let turn_prompt = result.expect("prompt rendering should succeed");
    assert!(turn_prompt.text.contains("\"src/main.rs\""));
    assert!(!turn_prompt.text.contains("@src/main.rs"));
    assert!(!turn_prompt.text.contains("looked/up/"));
}

#[test]
fn turn_prompt_for_runtime_preserves_generated_at_tokens_for_agent_data() {
    // Arrange
    let prompt = TurnPrompt::from_agent_data(
        "Review this diff:\n```diff\n+@dataclass\n+class Config:\n+    pass\n```".to_string(),
    );
    let request_kind = AgentRequestKind::UtilityPrompt;

    // Act
    let result = turn_prompt_for_runtime(
        prompt,
        &request_kind,
        None,
        InstructionDeliveryMode::BootstrapFull,
        &crate::channel::PersonalityPrompt::default(),
        ProtocolSchemaInstructionMode::PromptSchema,
        Path::new(TEST_WORKSPACE_ROOT),
    );

    // Assert
    let turn_prompt = result.expect("prompt rendering should succeed");
    assert!(turn_prompt.text.contains("+@dataclass"));
    assert!(!turn_prompt.text.contains("+\"dataclass\""));
}

#[test]
fn instruction_delivery_mode_for_runtime_reuses_matching_bootstrap_state() {
    // Arrange
    let request = AppServerTurnRequest {
        provider_call_budget: None,
        folder: PathBuf::from("/tmp/test"),
        live_transcript: None,
        main_checkout_root: None,
        model: "test-model".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("hello"),
        provider_conversation_id: Some("thread-123".to_string()),
        persisted_instruction_conversation_id:
            persisted_instruction_conversation_id_for_session_turn(Some("thread-123")),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionResume,
        replay_transcript: None,
        session_id: "test-session".to_string(),
        speed_mode: crate::model::session::SpeedMode::default(),
    };

    // Act
    let delivery_mode = instruction_delivery_mode_for_runtime(&request, Some("thread-123"), false);

    // Assert
    assert_eq!(delivery_mode, InstructionDeliveryMode::DeltaOnly);
}
