use std::path::Path;
use std::process::Command;

use super::app_server::build_gemini_acp_command;
use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};

/// Backend implementation for the Gemini ACP runtime.
pub(super) struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        Ok(())
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        let mut command = build_gemini_acp_command(request.folder, request.model);
        if request.permission_mode.is_read_only()
            && !matches!(
                request.request_kind,
                crate::channel::AgentRequestKind::UtilityPrompt
            )
        {
            command.arg("--approval-mode").arg("plan").arg("--sandbox");
        }

        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::channel::AgentRequestKind;
    use crate::model::agent::ReasoningLevel;

    /// Returns a utility request kind for Gemini command construction tests.
    fn utility_request_kind() -> AgentRequestKind {
        AgentRequestKind::UtilityPrompt
    }

    /// Returns a session request kind for Gemini command construction tests.
    fn session_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    #[test]
    fn test_gemini_setup_creates_no_files() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        AgentBackend::setup(&backend, temp_directory.path()).expect("setup should succeed");

        // Assert
        assert_eq!(
            std::fs::read_dir(temp_directory.path())
                .expect("failed to read dir")
                .count(),
            0
        );
    }

    #[test]
    /// Verifies Gemini startup uses the ACP runtime command shape.
    fn test_gemini_build_command_uses_acp_runtime_command() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "gemini-3.7-flash",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Generate title",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &utility_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(args, vec!["--acp", "--model", "gemini-3.7-flash"]);
        assert_eq!(command.get_current_dir(), Some(temp_directory.path()));
    }

    #[test]
    /// Verifies Gemini research turns combine ACP permission cancellation
    /// with the CLI's native sandboxed plan mode.
    fn test_gemini_read_only_command_uses_sandboxed_plan_mode() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "gemini-3.7-flash",
                permission_mode: crate::model::permission::PermissionMode::ReadOnly,
                personality_prompt: None,
                prompt: "Inspect the architecture",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            args,
            vec![
                "--acp",
                "--model",
                "gemini-3.7-flash",
                "--approval-mode",
                "plan",
                "--sandbox"
            ]
        );
    }

    #[test]
    /// Verifies read-only Gemini utility prompts avoid the plan-mode bootstrap
    /// while ACP permission cancellation continues to reject mutations.
    fn test_gemini_read_only_utility_command_uses_standard_acp_mode() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "gemini-3.7-flash",
                permission_mode: crate::model::permission::PermissionMode::ReadOnly,
                personality_prompt: None,
                prompt: "Review the supplied diff",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &utility_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(args, vec!["--acp", "--model", "gemini-3.7-flash"]);
    }
}
