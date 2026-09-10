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
                crate::channel::AgentRequestKind::FocusedReview
                    | crate::channel::AgentRequestKind::UtilityPrompt
            )
        {
            command.arg("--approval-mode").arg("plan").arg("--sandbox");
        }

        Ok(command)
    }
}

#[cfg(test)]
#[path = "gemini_test.rs"]
mod tests;
