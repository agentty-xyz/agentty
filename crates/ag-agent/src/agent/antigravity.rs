use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Stdio};

use ag_protocol::{SchemaRequiredPolicy, protocol_output_schema};

use super::availability;
use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};
use super::prompt::{self as shared_prompt, CliPromptAccessRootMode};

/// Wall-clock limit passed to Antigravity headless mode for one Agentty turn.
///
/// Antigravity CLI defaults print mode to five minutes, which is too short
/// for repository edits.
const ANTIGRAVITY_PRINT_TIMEOUT: &str = "1h";
/// Backend implementation for the Antigravity CLI.
///
/// Agentty starts `agy` in its persistent NDJSON input/output mode and sends
/// prompts through stdin. The provider runtime owns native conversation
/// history and context compaction, while Agentty persists the returned
/// conversation id for recovery after process or application restarts.
/// Agentty validates the installed CLI version during background discovery,
/// then checks the cached executable fingerprint during setup and before every
/// runtime start so persisted sessions cannot invoke an incompatible or
/// replaced executable.
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
        let BuildCommandRequest {
            attachments,
            folder,
            main_checkout_root: _main_checkout_root,
            model,
            permission_mode,
            prompt: _prompt,
            request_kind,
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

        command.arg("--sandbox");
        if permission_mode.is_read_only() {
            command.arg("--mode").arg("plan");
        } else {
            command.arg("--dangerously-skip-permissions");
        }
        command
            .arg("--print-timeout")
            .arg(ANTIGRAVITY_PRINT_TIMEOUT)
            .arg("--model")
            .arg(model)
            .arg("--effort")
            .arg(reasoning_level.antigravity())
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--json-schema")
            .arg(
                protocol_output_schema(
                    request_kind.protocol_profile(),
                    SchemaRequiredPolicy::MinimumProtocolKeys,
                )
                .to_string(),
            )
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(command)
    }
}

#[cfg(test)]
#[path = "antigravity_test.rs"]
mod tests;
