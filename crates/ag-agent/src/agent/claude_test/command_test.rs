use std::ffi::OsStr;
use std::path::PathBuf;

use ag_protocol::{ProtocolSchemaInstructionMode, TurnPromptAttachment};
use serde_json::Value;
use tempfile::tempdir;

use super::support::{session_start_request_kind, settings_argument, utility_request_kind};
use crate::agent::backend::{AgentBackend, BuildCommandRequest};
use crate::agent::claude::ClaudeBackend;
use crate::agent::prompt as shared_prompt;
use crate::model::agent::ReasoningLevel;

#[test]
/// Verifies Claude fast sessions enable the noninteractive `fastMode`
/// setting.
fn test_claude_fast_mode_sets_workspace_setting() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = ClaudeBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "claude-opus-5",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Respond quickly",
            reasoning_level: ReasoningLevel::default(),
            request_kind: &session_start_request_kind(),
            speed_mode: crate::model::session::SpeedMode::Fast,
        },
    )
    .expect("command should build");
    let settings = settings_argument(&command);

    // Assert
    assert_eq!(
        settings.get("fastMode").and_then(Value::as_bool),
        Some(true)
    );
}

#[test]
/// Verifies Claude commands pass the selected Opus 5 model through the
/// Claude Code model environment variable.
fn test_claude_command_sets_anthropic_model_to_claude_opus_5() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = ClaudeBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "claude-opus-5",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Use Opus",
            reasoning_level: ReasoningLevel::default(),
            request_kind: &session_start_request_kind(),
            speed_mode: crate::model::session::SpeedMode::default(),
        },
    )
    .expect("command should build");
    let anthropic_model = command
        .get_envs()
        .find(|(key, _value)| *key == OsStr::new("ANTHROPIC_MODEL"))
        .and_then(|(_key, value)| value)
        .map(|value| value.to_string_lossy().into_owned());

    // Assert
    assert_eq!(anthropic_model, Some("claude-opus-5".to_string()));
}

#[test]
/// Verifies Claude commands pass the selected Opus 4.8 model through the
/// Claude Code model environment variable.
fn test_claude_command_sets_anthropic_model_to_claude_opus_48() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = ClaudeBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "claude-opus-5",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Use Opus",
            reasoning_level: ReasoningLevel::default(),
            request_kind: &session_start_request_kind(),
            speed_mode: crate::model::session::SpeedMode::default(),
        },
    )
    .expect("command should build");
    let anthropic_model = command
        .get_envs()
        .find(|(key, _value)| *key == OsStr::new("ANTHROPIC_MODEL"))
        .and_then(|(_key, value)| value)
        .map(|value| value.to_string_lossy().into_owned());

    // Assert
    assert_eq!(anthropic_model, Some("claude-opus-5".to_string()));
}

#[test]
/// Verifies the `--effort` flag is passed to Claude with the correct value
/// for each `ReasoningLevel`.
fn test_claude_command_passes_effort_flag_for_each_reasoning_level() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = ClaudeBackend;
    let cases = [
        (ReasoningLevel::Low, "low"),
        (ReasoningLevel::Medium, "medium"),
        (ReasoningLevel::High, "high"),
        (ReasoningLevel::XHigh, "max"),
        (ReasoningLevel::Max, "max"),
    ];

    for (reasoning_level, expected_effort) in cases {
        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-sonnet-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Do work",
                reasoning_level,
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        // Assert
        let effort_pos = args
            .iter()
            .position(|arg| arg == "--effort")
            .expect("--effort flag should be present");
        assert_eq!(
            args[effort_pos + 1],
            expected_effort,
            "expected effort={expected_effort} for {reasoning_level:?}"
        );
    }
}

#[test]
/// Verifies Claude turns grant filesystem access to pasted-image parent
/// directories.
fn test_claude_command_adds_attachment_access_directories() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = ClaudeBackend;
    let attachments = vec![
        TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from("/tmp/agentty/images/one.png"),
        },
        TurnPromptAttachment {
            placeholder: "[Image #2]".to_string(),
            local_image_path: PathBuf::from("/tmp/agentty/images/two.png"),
        },
    ];

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            attachments: &attachments,
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "claude-sonnet-5",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Inspect [Image #1] and [Image #2]",
            reasoning_level: ReasoningLevel::default(),
            request_kind: &session_start_request_kind(),
            speed_mode: crate::model::session::SpeedMode::default(),
        },
    )
    .expect("command should build");
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        args.iter()
            .filter(|arg| arg.as_str() == "--add-dir")
            .count(),
        1
    );
    assert!(args.contains(&"/tmp/agentty/images".to_string()));
}

#[test]
/// Verifies Claude prompts include repo-root-relative path guidance.
fn test_claude_prompt_stdin_payload_includes_repo_root_path_instructions() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");

    // Act
    let prompt = String::from_utf8(
        shared_prompt::build_prompt_stdin_payload(
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-sonnet-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Plan prompt",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
            ProtocolSchemaInstructionMode::TransportSchema,
            "Claude",
        )
        .expect("prompt payload should build"),
    )
    .expect("prompt payload should be utf-8");

    // Assert
    assert!(prompt.contains("repository-root-relative POSIX paths"));
    assert!(prompt.contains("`path:line:column`"));
    assert!(!prompt.contains("summary"));
}

#[test]
/// Verifies one-shot Claude prompts keep protocol JSON guidance while
/// native schema enforcement carries the full response schema.
fn test_claude_one_shot_command_enforces_json_schema() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = ClaudeBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "claude-sonnet-5",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Generate title",
            reasoning_level: ReasoningLevel::default(),
            request_kind: &utility_request_kind(),
            speed_mode: crate::model::session::SpeedMode::default(),
        },
    )
    .expect("command should build");
    let debug_command = format!("{command:?}");
    let prompt = String::from_utf8(
        shared_prompt::build_prompt_stdin_payload(
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-sonnet-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Generate title",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &utility_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
            ProtocolSchemaInstructionMode::TransportSchema,
            "Claude",
        )
        .expect("prompt payload should build"),
    )
    .expect("prompt payload should be utf-8");

    // Assert
    assert!(prompt.contains("Structured response protocol:"));
    assert!(!prompt.contains("summary"));
    assert!(!prompt.contains("Authoritative JSON Schema:"));
    assert!(debug_command.contains("--output-format"));
    assert!(debug_command.contains("stream-json"));
    assert!(debug_command.contains("--json-schema"));
    assert!(debug_command.contains("--input-format"));
}

#[test]
/// Verifies structured Claude commands include native JSON schema
/// validation.
fn test_claude_start_command_includes_json_schema() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = ClaudeBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: None,
            replay_transcript: None,
            model: "claude-sonnet-5",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Return protocol response",
            reasoning_level: ReasoningLevel::default(),
            request_kind: &session_start_request_kind(),
            speed_mode: crate::model::session::SpeedMode::default(),
        },
    )
    .expect("command should build");
    let debug_command = format!("{command:?}");
    let prompt = String::from_utf8(
        shared_prompt::build_prompt_stdin_payload(
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-sonnet-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Return protocol response",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
            ProtocolSchemaInstructionMode::TransportSchema,
            "Claude",
        )
        .expect("prompt payload should build"),
    )
    .expect("prompt payload should be utf-8");

    // Assert
    assert!(debug_command.contains("--json-schema"));
    assert!(debug_command.contains("AgentResponse"));
    assert!(prompt.contains("Structured response protocol:"));
    assert!(!prompt.contains("summary"));
    assert!(!prompt.contains("Authoritative JSON Schema:"));
}
