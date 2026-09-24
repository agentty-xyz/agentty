use ag_contracts::AgentRequestKind;
use tempfile::tempdir;

use crate::agent::backend::{AgentBackend, BuildCommandRequest};
use crate::agent::codex::CodexBackend;

fn session_start_request_kind() -> AgentRequestKind {
    AgentRequestKind::SessionStart
}

fn session_resume_request_kind(_replay_transcript: Option<&str>) -> AgentRequestKind {
    AgentRequestKind::SessionResume
}

/// Verifies Codex start requests build the persistent app-server command.
#[test]
fn build_command_builds_app_server_runtime_for_start_requests() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = CodexBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            execution_policy: &ag_contracts::ExecutionPolicy {
                max_concurrent_subagents: std::num::NonZeroUsize::new(5),
                ..ag_contracts::ExecutionPolicy::default()
            },
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "gpt-6-sol",
            permission_mode: ag_contracts::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Run checks",
            reasoning_level: ag_contracts::ReasoningLevel::High,
            request_kind: &session_start_request_kind(),
            speed_mode: ag_contracts::SpeedMode::default(),
        },
    )
    .expect("command build should succeed");
    let debug_command = format!("{command:?}");

    // Assert
    assert!(debug_command.contains("agents.max_concurrent_threads_per_session=5"));
    assert!(debug_command.contains("codex"));
    assert!(debug_command.contains("app-server"));
    assert!(debug_command.contains("stdio://"));
}

/// Verifies resume requests reuse the same Codex runtime launch command.
#[test]
fn build_command_builds_app_server_runtime_for_resume_requests() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = CodexBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            execution_policy: &ag_contracts::ExecutionPolicy::default(),
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "gpt-6-sol",
            permission_mode: ag_contracts::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Continue edits",
            reasoning_level: ag_contracts::ReasoningLevel::High,
            request_kind: &session_resume_request_kind(Some("previous assistant output")),
            speed_mode: ag_contracts::SpeedMode::default(),
        },
    )
    .expect("resume command build should succeed");
    let arguments = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        arguments,
        vec!["--model", "gpt-6-sol", "app-server", "--listen", "stdio://"]
    );
}

/// Verifies `gpt-6-luna` is forwarded to the Codex app-server command.
#[test]
fn build_command_accepts_gpt_56_luna_model() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = CodexBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            execution_policy: &ag_contracts::ExecutionPolicy::default(),
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: ag_session::AgentModel::Gpt6Luna.as_str(),
            permission_mode: ag_contracts::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Run a quick edit",
            reasoning_level: ag_contracts::ReasoningLevel::Medium,
            request_kind: &session_start_request_kind(),
            speed_mode: ag_contracts::SpeedMode::default(),
        },
    )
    .expect("luna model command build should succeed");
    let arguments = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        arguments,
        vec![
            "--model",
            "gpt-6-luna",
            "app-server",
            "--listen",
            "stdio://"
        ]
    );
}

/// Verifies utility prompts use the same app-server runtime launch path.
#[test]
fn build_command_builds_app_server_runtime_for_utility_prompts() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = CodexBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            execution_policy: &ag_contracts::ExecutionPolicy::default(),
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "gpt-6-sol",
            permission_mode: ag_contracts::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Generate title",
            reasoning_level: ag_contracts::ReasoningLevel::Low,
            request_kind: &AgentRequestKind::UtilityPrompt,
            speed_mode: ag_contracts::SpeedMode::default(),
        },
    )
    .expect("utility command build should succeed");

    // Assert
    assert_eq!(command.get_current_dir(), Some(temp_directory.path()));
}
