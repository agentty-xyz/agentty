use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Stdio};

use ag_protocol::{
    ProtocolSchemaInstructionMode, SchemaRequiredPolicy, agent_response_output_schema_json,
};

use super::availability;
use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};
use super::prompt::{self as shared_prompt, CliPromptAccessRootMode};

/// Wall-clock limit passed with `agy --print` for one Agentty turn.
///
/// Antigravity CLI defaults print mode to five minutes, which is too short
/// for repository edits.
const ANTIGRAVITY_PRINT_TIMEOUT: &str = "1h";
/// Conservative upper bound for the prompt passed through `agy --print`.
///
/// Antigravity exposes the print prompt only as one command argument. Keeping
/// the argument below 32 KiB avoids platform-specific spawn failures while
/// leaving room for the remaining command arguments and environment.
const ANTIGRAVITY_MAX_PROMPT_BYTES: usize = 32 * 1024;

/// Backend implementation for the Antigravity CLI.
///
/// Antigravity does not currently expose an ACP/app-server flag in `agy
/// --help`, so Agentty runs it as a stateless CLI provider through
/// `agy --print`. Supported Antigravity releases define `--print` as a
/// string-valued flag, so Agentty passes the rendered prompt as its immediately
/// following argument. All other options precede `--print` so none can be
/// consumed as prompt text. Agentty validates the installed CLI version during
/// background discovery, then checks the cached executable fingerprint during
/// setup and before every turn so persisted sessions cannot invoke an
/// incompatible or replaced executable.
pub(super) struct AntigravityBackend {
    path_value: Option<OsString>,
    validate_cached_cli: fn(Option<&OsStr>) -> Result<(), String>,
}

impl AntigravityBackend {
    /// Creates the production backend with real CLI compatibility checks.
    pub(super) fn new() -> Self {
        Self {
            path_value: std::env::var_os("PATH"),
            validate_cached_cli: availability::ensure_cached_antigravity_cli_supported_on_path,
        }
    }
}

impl AgentBackend for AntigravityBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        (self.validate_cached_cli)(self.path_value.as_deref()).map_err(AgentBackendError::Setup)
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        (self.validate_cached_cli)(self.path_value.as_deref())
            .map_err(AgentBackendError::CommandBuild)?;
        let prompt = shared_prompt::build_cli_prompt_text(
            request,
            ProtocolSchemaInstructionMode::TransportSchema,
            "Antigravity",
        )?;
        if prompt.len() > ANTIGRAVITY_MAX_PROMPT_BYTES {
            return Err(AgentBackendError::CommandBuild(format!(
                "Antigravity prompt is {} bytes, exceeding Agentty's safe \
                 {ANTIGRAVITY_MAX_PROMPT_BYTES}-byte command argument limit. Start a new \
                 Antigravity session to avoid replaying its transcript, or shorten the prompt.",
                prompt.len()
            )));
        }
        let BuildCommandRequest {
            attachments,
            folder,
            main_checkout_root: _main_checkout_root,
            model,
            prompt: _prompt,
            request_kind: _request_kind,
            replay_transcript: _replay_transcript,
            reasoning_level,
            ..
        } = request;
        let mut command = Command::new("agy");

        shared_prompt::append_cli_prompt_access_directories(
            &mut command,
            folder,
            attachments,
            CliPromptAccessRootMode::WorkspaceThenAttachments,
        );

        command
            .arg("--sandbox")
            .arg("--dangerously-skip-permissions")
            .arg("--print-timeout")
            .arg(ANTIGRAVITY_PRINT_TIMEOUT)
            .arg("--model")
            .arg(model)
            .arg("--effort")
            .arg(reasoning_level.antigravity())
            .arg("--output-format")
            .arg("stream-json")
            .arg("--json-schema")
            .arg(agent_response_output_schema_json(
                SchemaRequiredPolicy::MinimumProtocolKeys,
            ))
            .arg("--print")
            .arg(prompt)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    use ag_protocol::TurnPromptAttachment;
    use serde_json::Value;
    use tempfile::{TempDir, tempdir};

    use super::shared_prompt::{
        CliPromptAccessRootMode, build_cli_prompt_text, cli_prompt_access_directories,
    };
    use super::*;
    use crate::channel::AgentRequestKind;
    use crate::model::agent::{AgentModel, ReasoningLevel};

    fn session_resume_request_kind(_replay_transcript: Option<&str>) -> AgentRequestKind {
        AgentRequestKind::SessionResume
    }

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    /// Creates a backend whose compatibility boundary accepts the test CLI.
    fn supported_backend() -> AntigravityBackend {
        AntigravityBackend {
            path_value: None,
            validate_cached_cli: |_| Ok(()),
        }
    }

    /// Creates a temp directory whose own basename is visible so command
    /// assertions are stable on platforms where `tempdir()` uses dot prefixes.
    fn visible_tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("agentty-antigravity-test-")
            .tempdir()
            .expect("failed to create visible temp dir")
    }

    #[test]
    /// Verifies Antigravity starts in native structured print mode with every
    /// option before the string-valued `--print` flag and its prompt.
    fn test_antigravity_build_command_uses_print_mode_with_sandbox() {
        // Arrange
        let temp_directory = visible_tempdir();
        let backend = supported_backend();
        let requested_model = AgentModel::Gemini31Pro.provider_model_str();

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: requested_model,
                personality_prompt: None,
                prompt: "Write tests",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let session_folder = temp_directory.path().to_string_lossy().into_owned();

        // Assert
        assert_eq!(
            args,
            vec![
                "--add-dir".to_string(),
                session_folder,
                "--sandbox".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--print-timeout".to_string(),
                ANTIGRAVITY_PRINT_TIMEOUT.to_string(),
                "--model".to_string(),
                requested_model.to_string(),
                "--effort".to_string(),
                ReasoningLevel::default().antigravity().to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--json-schema".to_string(),
                args[args.len() - 3].clone(),
                "--print".to_string(),
                args[args.len() - 1].clone(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(temp_directory.path()));
        let schema: Value =
            serde_json::from_str(&args[args.len() - 3]).expect("schema should be JSON");
        assert_eq!(schema["required"], serde_json::json!(["answer"]));
        assert!(args.last().is_some_and(|prompt| {
            prompt.contains("Structured response protocol:") && prompt.contains("Write tests")
        }));
    }

    #[test]
    /// Verifies Antigravity receives every supported effort and caps higher
    /// Agentty reasoning levels at the CLI's highest accepted value.
    fn test_antigravity_build_command_passes_supported_effort() {
        // Arrange
        let temp_directory = visible_tempdir();
        let backend = supported_backend();
        let cases = ReasoningLevel::ALL;

        for reasoning_level in cases {
            // Act
            let command = AgentBackend::build_command(
                &backend,
                BuildCommandRequest {
                    attachments: &[],
                    folder: temp_directory.path(),
                    main_checkout_root: None,
                    replay_transcript: None,
                    model: AgentModel::Gemini31Pro.provider_model_str(),
                    personality_prompt: None,
                    prompt: "Write tests",
                    reasoning_level,
                    request_kind: &session_start_request_kind(),
                },
            )
            .expect("command should build");
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let effort_position = args
                .iter()
                .position(|arg| arg == "--effort")
                .expect("--effort flag should be present");

            // Assert
            assert_eq!(args[effort_position + 1], reasoning_level.antigravity());
        }
    }

    #[test]
    /// Verifies Antigravity receives the real session worktree even when its
    /// path contains a hidden component.
    fn test_antigravity_build_command_uses_hidden_session_folder_directly() {
        // Arrange
        let temp_directory = visible_tempdir();
        let session_folder = temp_directory
            .path()
            .join(".agentty")
            .join("wt")
            .join("00cbfefe");
        std::fs::create_dir_all(&session_folder).expect("failed to create hidden session folder");
        let backend = supported_backend();
        let requested_model = AgentModel::Gemini31Pro.provider_model_str();

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: &session_folder,
                main_checkout_root: None,
                replay_transcript: None,
                model: requested_model,
                personality_prompt: None,
                prompt: "Write tests",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let session_folder_display = session_folder.to_string_lossy().into_owned();

        // Assert
        assert_eq!(command.get_current_dir(), Some(session_folder.as_path()));
        assert_eq!(args[0], "--add-dir");
        assert_eq!(args[1], session_folder_display);
    }

    #[test]
    /// Verifies modern Antigravity setup does not mutate repository metadata.
    fn test_antigravity_setup_does_not_create_git_excludes() {
        // Arrange
        let temp_directory = visible_tempdir();
        let backend = supported_backend();

        // Act
        AgentBackend::setup(&backend, temp_directory.path()).expect("setup should succeed");

        // Assert
        assert!(!temp_directory.path().join(".git").exists());
    }

    #[test]
    /// Verifies setup surfaces the compatibility probe's actionable error.
    fn test_antigravity_setup_rejects_unsupported_cli() {
        // Arrange
        let temp_directory = visible_tempdir();
        let backend = AntigravityBackend {
            path_value: None,
            validate_cached_cli: |_| Err("Run `agy update`, then retry.".to_string()),
        };

        // Act
        let error = AgentBackend::setup(&backend, temp_directory.path())
            .expect_err("unsupported Antigravity should fail setup");

        // Assert
        assert_eq!(
            error,
            AgentBackendError::Setup("Run `agy update`, then retry.".to_string())
        );
    }

    #[test]
    /// Verifies every turn surfaces the cached compatibility error without
    /// launching another version subprocess.
    fn test_antigravity_build_command_rejects_cached_cli_error() {
        // Arrange
        let temp_directory = visible_tempdir();
        let backend = AntigravityBackend {
            path_value: None,
            validate_cached_cli: |_| Err("Run `agy update`, then retry.".to_string()),
        };

        // Act
        let error = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: AgentModel::Gemini31Pro.provider_model_str(),
                personality_prompt: None,
                prompt: "Write tests",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
            },
        )
        .expect_err("unsupported Antigravity should fail the turn");

        // Assert
        assert_eq!(
            error,
            AgentBackendError::CommandBuild("Run `agy update`, then retry.".to_string())
        );
    }

    #[test]
    /// Verifies a resumed transcript cannot exceed the safe size for the
    /// string-valued `--print` command argument.
    fn test_antigravity_build_command_rejects_oversized_replay_prompt() {
        // Arrange
        let temp_directory = visible_tempdir();
        let backend = supported_backend();
        let replay_transcript = "x".repeat(ANTIGRAVITY_MAX_PROMPT_BYTES);
        let request_kind = session_resume_request_kind(Some(&replay_transcript));

        // Act
        let error = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: Some(&replay_transcript),
                model: AgentModel::Gemini31Pro.provider_model_str(),
                personality_prompt: None,
                prompt: "Continue work",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &request_kind,
            },
        )
        .expect_err("oversized Antigravity prompt should fail before spawning");

        // Assert
        assert!(matches!(&error, AgentBackendError::CommandBuild(_)));
        let message = error.to_string();
        assert!(message.contains("safe 32768-byte command argument limit"));
        assert!(message.contains("Start a new Antigravity session"));
    }

    #[cfg(unix)]
    #[test]
    /// Verifies Antigravity command construction preserves prompt-rendering
    /// errors for attachment paths that cannot be represented as UTF-8.
    fn test_antigravity_build_command_rejects_non_utf8_attachment_path() {
        // Arrange
        let temp_directory = visible_tempdir();
        let backend = supported_backend();
        let attachment = TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: OsString::from_vec(vec![0x66, 0x80, 0x6f]).into(),
        };

        // Act
        let error = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[attachment],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: AgentModel::Gemini31Pro.provider_model_str(),
                personality_prompt: None,
                prompt: "Review [Image #1]",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
            },
        )
        .expect_err("non-UTF-8 attachment path should fail command construction");

        // Assert
        assert_eq!(
            error,
            AgentBackendError::CommandBuild(
                "Antigravity prompt image path is not valid UTF-8".to_string()
            )
        );
    }

    #[test]
    /// Verifies Antigravity grants workspace roots for the session folder and
    /// external prompt image attachments.
    fn test_antigravity_build_command_adds_workspace_directories() {
        // Arrange
        let temp_directory = visible_tempdir();
        let attachment_directory = temp_directory.path().join("images");
        let attachment = TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: attachment_directory.join("one.png"),
        };
        let backend = supported_backend();
        let requested_model = "gemini-3.6-flash";

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[attachment],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: requested_model,
                personality_prompt: None,
                prompt: "Review [Image #1]",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let expected_workspace_args = vec![
            "--add-dir".to_string(),
            temp_directory.path().to_string_lossy().into_owned(),
            "--add-dir".to_string(),
            attachment_directory.to_string_lossy().into_owned(),
        ];
        let expected_model_args = ["--model".to_string(), requested_model.to_string()];

        // Assert
        assert_eq!(args[..4], expected_workspace_args);
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == expected_model_args[0] && pair[1] == expected_model_args[1]),
            "command should include requested model"
        );
    }

    #[test]
    /// Verifies Antigravity keeps the session worktree as the primary
    /// workspace even when an attachment directory sorts before it.
    fn test_workspace_access_directories_keep_session_folder_first() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let session_folder = temp_directory.path().join("z-session");
        let attachment_directory = temp_directory.path().join("a-images");
        let attachment = TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: attachment_directory.join("one.png"),
        };

        // Act
        let workspace_directories = cli_prompt_access_directories(
            &session_folder,
            &[attachment],
            CliPromptAccessRootMode::WorkspaceThenAttachments,
        );

        // Assert
        assert_eq!(
            workspace_directories,
            vec![session_folder, attachment_directory]
        );
    }

    #[test]
    /// Verifies Antigravity argv prompts include protocol instructions and
    /// replayed transcript output for resume turns.
    fn test_antigravity_argv_prompt_replays_transcript_on_resume() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let request_kind = session_resume_request_kind(Some("previous answer"));

        // Act
        let prompt = build_cli_prompt_text(
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: Some("previous answer"),
                model: AgentModel::Gemini31Pro.provider_model_str(),
                personality_prompt: None,
                prompt: "continue work",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &request_kind,
            },
            ProtocolSchemaInstructionMode::TransportSchema,
            "Antigravity",
        )
        .expect("argv prompt should build");

        // Assert
        assert!(prompt.contains("Structured response protocol:"));
        assert!(!prompt.contains("\"$schema\""));
        assert!(prompt.contains("previous answer"));
        assert!(prompt.contains("continue work"));
    }
}
