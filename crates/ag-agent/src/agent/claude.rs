use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};
use super::prompt::{CliPromptAccessRootMode, append_cli_prompt_access_directories};
use crate::agent::protocol::agent_response_output_schema_json;

/// Lists the Claude tools Agentty enables for unattended sessions.
///
/// `Bash` remains available for Claude workflows that need shell commands,
/// while Agentty keeps the process working directory scoped to the session
/// worktree.
const CLAUDE_ALLOWED_TOOLS: &str = "Bash,Edit,MultiEdit,Write,EnterPlanMode,ExitPlanMode";

/// Backend implementation for the Claude CLI.
///
/// Commands are built with `--strict-mcp-config` so provider-level MCP
/// connector defaults (for example Claude.ai account connectors) are ignored
/// unless explicitly configured by Agentty. Claude runs in `stream-json` mode
/// so progress and tool-use events can surface live while the final turn still
/// honors native schema validation.
pub(super) struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        // Claude Code needs no config files
        Ok(())
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        let BuildCommandRequest {
            attachments,
            folder,
            request_kind,
            model,
            prompt: _prompt,
            reasoning_level,
        } = request;
        let mut command = Command::new("claude");

        if request_kind.is_resume() {
            command.arg("-c");
        }

        append_cli_prompt_access_directories(
            &mut command,
            folder,
            attachments,
            CliPromptAccessRootMode::AttachmentsOnly,
        );

        command.arg("-p");
        command.arg("--allowedTools").arg(CLAUDE_ALLOWED_TOOLS);
        command.arg("--input-format").arg("text");
        command.arg("--strict-mcp-config");
        command.arg("--verbose");
        command.arg("--effort").arg(reasoning_level.claude());
        command.arg("--output-format").arg("stream-json");
        command
            .arg("--json-schema")
            .arg(agent_response_output_schema_json());
        command
            .env("ANTHROPIC_MODEL", model)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;
    use crate::agent::prompt::{self as shared_prompt, ProtocolSchemaInstructionMode};
    use crate::channel::AgentRequestKind;
    use crate::model::agent::ReasoningLevel;
    use crate::model::turn_prompt::TurnPromptAttachment;

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    fn utility_request_kind() -> AgentRequestKind {
        AgentRequestKind::UtilityPrompt
    }

    #[test]
    /// Verifies Claude sessions allow Agentty's required write-capable tools.
    fn test_claude_auto_edit_mode_uses_write_capable_allowed_tools() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Plan prompt",
                request_kind: &session_start_request_kind(),
                model: "claude-sonnet-5",
                reasoning_level: ReasoningLevel::default(),
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
        assert!(debug_command.contains("--strict-mcp-config"));
        assert!(debug_command.contains("--effort"));
        assert!(debug_command.contains("--output-format"));
        assert!(debug_command.contains("stream-json"));
        assert!(!debug_command.contains("--permission-mode"));
        assert!(!args.iter().any(String::is_empty));
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
                prompt: "Use Opus",
                request_kind: &session_start_request_kind(),
                model: "claude-opus-4-8",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let anthropic_model = command
            .get_envs()
            .find(|(key, _value)| *key == OsStr::new("ANTHROPIC_MODEL"))
            .and_then(|(_key, value)| value)
            .map(|value| value.to_string_lossy().into_owned());

        // Assert
        assert_eq!(anthropic_model, Some("claude-opus-4-8".to_string()));
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
        ];

        for (reasoning_level, expected_effort) in cases {
            // Act
            let command = AgentBackend::build_command(
                &backend,
                BuildCommandRequest {
                    attachments: &[],
                    folder: temp_directory.path(),
                    prompt: "Do work",
                    request_kind: &session_start_request_kind(),
                    model: "claude-sonnet-5",
                    reasoning_level,
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
                prompt: "Inspect [Image #1] and [Image #2]",
                request_kind: &session_start_request_kind(),
                model: "claude-sonnet-5",
                reasoning_level: ReasoningLevel::default(),
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
                    prompt: "Plan prompt",
                    request_kind: &session_start_request_kind(),
                    model: "claude-sonnet-5",
                    reasoning_level: ReasoningLevel::default(),
                },
                ProtocolSchemaInstructionMode::TransportSchema,
                "Claude",
            )
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(prompt.contains("repository-root-relative POSIX paths"));
        assert!(prompt.contains("Paths must be relative to the repository root."));
        assert!(prompt.contains("summary"));
    }

    #[test]
    /// Verifies one-shot Claude prompts keep protocol JSON guidance while
    /// native schema enforcement carries the full response schema.
    fn test_claude_one_shot_command_enforces_json_schema_without_summary_prose() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Generate title",
                request_kind: &utility_request_kind(),
                model: "claude-sonnet-5",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let prompt = String::from_utf8(
            shared_prompt::build_prompt_stdin_payload(
                BuildCommandRequest {
                    attachments: &[],
                    folder: temp_directory.path(),
                    prompt: "Generate title",
                    request_kind: &utility_request_kind(),
                    model: "claude-sonnet-5",
                    reasoning_level: ReasoningLevel::default(),
                },
                ProtocolSchemaInstructionMode::TransportSchema,
                "Claude",
            )
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(prompt.contains("Structured response protocol:"));
        assert!(prompt.contains("summary"));
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
                prompt: "Return protocol response",
                request_kind: &session_start_request_kind(),
                model: "claude-sonnet-5",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let prompt = String::from_utf8(
            shared_prompt::build_prompt_stdin_payload(
                BuildCommandRequest {
                    attachments: &[],
                    folder: temp_directory.path(),
                    prompt: "Return protocol response",
                    request_kind: &session_start_request_kind(),
                    model: "claude-sonnet-5",
                    reasoning_level: ReasoningLevel::default(),
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
        assert!(prompt.contains("summary"));
        assert!(!prompt.contains("Authoritative JSON Schema:"));
    }
}
