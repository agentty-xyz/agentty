use tempfile::tempdir;

use crate::agent::backend::{AgentBackend, BuildCommandRequest};
use crate::agent::codex::CodexBackend;
use crate::channel::AgentRequestKind;

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
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "gpt-5.6-sol",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Run checks",
            reasoning_level: crate::model::agent::ReasoningLevel::High,
            request_kind: &session_start_request_kind(),
            speed_mode: crate::model::session::SpeedMode::default(),
        },
    )
    .expect("command build should succeed");
    let debug_command = format!("{command:?}");

    // Assert
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
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "gpt-5.6-sol",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Continue edits",
            reasoning_level: crate::model::agent::ReasoningLevel::High,
            request_kind: &session_resume_request_kind(Some("previous assistant output")),
            speed_mode: crate::model::session::SpeedMode::default(),
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
        vec![
            "--model",
            "gpt-5.6-sol",
            "-c",
            "agents.max_concurrent_threads_per_session=2",
            "app-server",
            "--listen",
            "stdio://"
        ]
    );
}

/// Verifies `gpt-5.6-luna` is forwarded to the Codex app-server command.
#[test]
fn build_command_accepts_gpt_56_luna_model() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = CodexBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: crate::model::agent::AgentModel::Gpt56Luna.as_str(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Run a quick edit",
            reasoning_level: crate::model::agent::ReasoningLevel::Medium,
            request_kind: &session_start_request_kind(),
            speed_mode: crate::model::session::SpeedMode::default(),
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
            "gpt-5.6-luna",
            "-c",
            "agents.max_concurrent_threads_per_session=2",
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
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "gpt-5.6-sol",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Generate title",
            reasoning_level: crate::model::agent::ReasoningLevel::Low,
            request_kind: &AgentRequestKind::UtilityPrompt,
            speed_mode: crate::model::session::SpeedMode::default(),
        },
    )
    .expect("utility command build should succeed");

    // Assert
    assert_eq!(command.get_current_dir(), Some(temp_directory.path()));
}
