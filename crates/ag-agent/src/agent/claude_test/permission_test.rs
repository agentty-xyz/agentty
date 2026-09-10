use std::ffi::OsStr;
use std::path::Path;

use serde_json::Value;
use tempfile::tempdir;

use super::support::{session_start_request_kind, settings_argument};
use crate::agent::backend::{AgentBackend, BuildCommandRequest};
use crate::agent::claude::{
    CLAUDE_ALLOWED_TOOLS, CLAUDE_READ_ONLY_TOOLS, ClaudeBackend, claude_absolute_permission_path,
};
use crate::model::agent::ReasoningLevel;

#[test]
/// Verifies Claude permission-rule paths use slash separators for glob
/// matching even when given a Windows-style checkout path.
fn test_claude_absolute_permission_path_normalizes_windows_separators() {
    // Arrange
    let path = Path::new(r"C:\Users\dev\project");

    // Act
    let rule_path = claude_absolute_permission_path(path);

    // Assert
    assert_eq!(rule_path, "//C:/Users/dev/project");
    assert!(!rule_path.contains('\\'));
}

#[test]
/// Verifies Claude sessions allow Agentty's required edit and web tools.
fn test_claude_auto_edit_mode_uses_write_capable_allowed_tools() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let main_checkout_root = temp_directory.path().join("main");
    let backend = ClaudeBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: Some(main_checkout_root.as_path()),
            replay_transcript: None,
            model: "claude-sonnet-5",
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality_prompt: None,
            prompt: "Plan prompt",
            reasoning_level: ReasoningLevel::default(),
            request_kind: &session_start_request_kind(),
            speed_mode: crate::model::session::SpeedMode::default(),
        },
    )
    .expect("command should build");
    let debug_command = format!("{command:?}");
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    // Assert
    assert!(debug_command.contains("--allowedTools"));
    assert!(debug_command.contains(CLAUDE_ALLOWED_TOOLS));
    assert!(debug_command.contains("Bash"));
    assert!(debug_command.contains("MultiEdit"));
    assert!(debug_command.contains("Write"));
    assert!(debug_command.contains("WebSearch"));
    assert!(debug_command.contains("WebFetch"));
    assert!(debug_command.contains("--strict-mcp-config"));
    assert!(debug_command.contains("--settings"));
    assert!(debug_command.contains("--effort"));
    assert!(debug_command.contains("--output-format"));
    assert!(debug_command.contains("stream-json"));
    assert!(!debug_command.contains("--permission-mode"));
    assert!(!args.iter().any(String::is_empty));
    assert!(command.get_envs().any(|(key, value)| {
        key == "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS" && value == Some(OsStr::new("2"))
    }));

    let settings = settings_argument(&command);
    assert_eq!(
        settings.get("fastMode").and_then(Value::as_bool),
        Some(false)
    );
    let deny_rules = settings
        .pointer("/permissions/deny")
        .and_then(Value::as_array)
        .expect("deny rules should be present");
    assert!(deny_rules.iter().any(|rule| {
        rule.as_str()
            .is_some_and(|rule| rule.starts_with("Edit(//") && rule.ends_with("/main/**)"))
    }));
    assert_eq!(
        settings
            .pointer("/sandbox/enabled")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(settings.pointer("/sandbox/network").is_none());
}

#[test]
/// Verifies Claude research turns expose only non-mutating tools and a
/// read-only sandbox.
fn test_claude_read_only_mode_uses_plan_tools_and_denies_writes() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let main_checkout_root = temp_directory.path().join("main");
    let backend = ClaudeBackend;

    // Act
    let command = AgentBackend::build_command(
        &backend,
        BuildCommandRequest {
            attachments: &[],
            folder: temp_directory.path(),
            main_checkout_root: Some(main_checkout_root.as_path()),
            replay_transcript: None,
            model: "claude-sonnet-5",
            permission_mode: crate::model::permission::PermissionMode::ReadOnly,
            personality_prompt: None,
            prompt: "Inspect the architecture",
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
    let flag_value = |flag: &str| {
        let position = args
            .iter()
            .position(|argument| argument == flag)
            .expect("flag should be present");

        args[position + 1].as_str()
    };
    let settings = settings_argument(&command);

    // Assert
    assert_eq!(flag_value("--tools"), CLAUDE_READ_ONLY_TOOLS);
    assert_eq!(flag_value("--allowedTools"), CLAUDE_READ_ONLY_TOOLS);
    assert_eq!(flag_value("--permission-mode"), "plan");
    assert!(!CLAUDE_READ_ONLY_TOOLS.contains("Bash"));
    assert!(!CLAUDE_READ_ONLY_TOOLS.contains("Write"));
    assert_eq!(
        settings.pointer("/sandbox/filesystem/allowWrite"),
        Some(&serde_json::json!([]))
    );
    assert_eq!(
        settings
            .pointer("/sandbox/network/allowLocalBinding")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        settings
            .pointer("/sandbox/allowUnsandboxedCommands")
            .and_then(Value::as_bool),
        Some(false)
    );
}
