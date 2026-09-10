use std::path::Path;
use std::process::Command;

use super::app_server::build_codex_app_server_command;
use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};

/// Keeps Codex setup wired through [`AgentBackend`] while always routing turns
/// through the Codex app-server runtime.
///
/// Codex session turns and one-shot utility prompts run on top of
/// `codex app-server`, so `build_command()` constructs the long-lived runtime
/// process command instead of a one-shot CLI prompt invocation. Runtime
/// permission and sandbox policies are sent later through Codex app-server
/// JSON-RPC payloads in `app_server/codex/policy.rs`.
pub(super) struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        // Codex CLI needs no config files
        Ok(())
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        Ok(build_codex_app_server_command(
            request.folder,
            request.model,
        ))
    }
}

#[cfg(test)]
#[path = "codex_test.rs"]
mod tests;
