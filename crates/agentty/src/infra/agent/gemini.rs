use std::path::Path;
use std::process::Command;

use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};

/// Backend implementation for the Gemini ACP runtime.
pub(super) struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        // Gemini CLI needs no config files
        Ok(())
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        Ok(build_app_server_command(request))
    }
}

/// Builds the persistent Gemini ACP runtime command for one session.
///
/// Prompt submission and resume behavior happen over ACP after the process is
/// running, so startup only depends on the working directory and model.
fn build_app_server_command(request: BuildCommandRequest<'_>) -> Command {
    let BuildCommandRequest {
        attachments: _attachments,
        folder,
        prompt: _prompt,
        request_kind: _request_kind,
        model,
        reasoning_level: _reasoning_level,
    } = request;
    let mut command = Command::new("gemini");
    command
        .arg("--acp")
        .arg("--model")
        .arg(model)
        .current_dir(folder);

    command
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::infra::channel::AgentRequestKind;

    fn utility_request_kind() -> AgentRequestKind {
        AgentRequestKind::UtilityPrompt
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
                prompt: "Generate title",
                request_kind: &utility_request_kind(),
                model: "gemini-3-flash-preview",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(args, vec!["--acp", "--model", "gemini-3-flash-preview"]);
        assert_eq!(command.get_current_dir(), Some(temp_directory.path()));
    }
}
