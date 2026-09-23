use std::path::Path;
use std::process::{Command, Stdio};

use ag_protocol::{SchemaRequiredPolicy, protocol_output_schema};

use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};
use super::prompt::{CliPromptAccessRootMode, append_cli_prompt_access_directories};
use crate::model::{reasoning, session};

/// Backend implementation for the Claude CLI.
///
/// Worker policies can select `--strict-mcp-config` so provider-level MCP
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
        super::execution_policy::validate(ag_session::AgentKind::Claude, request.execution_policy)?;
        let BuildCommandRequest {
            execution_policy,
            attachments,
            folder,
            main_checkout_root,
            model,
            permission_mode,
            request_kind,
            prompt: _prompt,
            replay_transcript: _replay_transcript,
            reasoning_level,
            speed_mode,
            ..
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
        super::execution_policy::apply_claude_tools(
            &mut command,
            &execution_policy.tools,
            permission_mode,
        );
        append_claude_workspace_settings(
            &mut command,
            folder,
            main_checkout_root,
            permission_mode,
            speed_mode,
        );
        command
            .arg("--append-system-prompt")
            .arg(ag_protocol::workspace_instructions(folder));
        command.arg("--input-format").arg("text");
        if execution_policy.mcp == ag_contracts::McpPolicy::Disabled {
            command.arg("--strict-mcp-config");
        }
        command.arg("--verbose");
        command
            .arg("--effort")
            .arg(reasoning::claude(reasoning_level));
        command.arg("--output-format").arg("stream-json");
        command.arg("--json-schema").arg(
            protocol_output_schema(
                request_kind.protocol_profile(),
                SchemaRequiredPolicy::MinimumProtocolKeys,
            )
            .to_string(),
        );
        command
            .env("ANTHROPIC_MODEL", model)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(limit) = execution_policy.max_concurrent_subagents {
            command.env("CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS", limit.to_string());
        }

        Ok(command)
    }
}

/// Appends per-turn Claude Code settings that keep known non-session
/// checkouts read-only.
fn append_claude_workspace_settings(
    command: &mut Command,
    workspace_folder: &Path,
    main_checkout_root: Option<&Path>,
    permission_mode: ag_contracts::PermissionMode,
    speed_mode: ag_contracts::SpeedMode,
) {
    let mut deny_rules = Vec::new();
    let mut deny_write_paths = Vec::new();
    if let Some(main_checkout_root) = main_checkout_root {
        let main_checkout_rule_path = claude_absolute_permission_path(main_checkout_root);
        deny_rules.push(format!("Edit({main_checkout_rule_path}/**)"));
        deny_write_paths.push(main_checkout_root.to_string_lossy().into_owned());
    }

    let allow_write_paths = if permission_mode.is_read_only() {
        deny_rules.push(format!(
            "Edit({}/**)",
            claude_absolute_permission_path(workspace_folder)
        ));
        deny_write_paths.push(workspace_folder.to_string_lossy().into_owned());
        Vec::new()
    } else {
        vec![workspace_folder.to_string_lossy().into_owned()]
    };
    let mut sandbox = serde_json::json!({
        "enabled": true,
        "filesystem": {
            "allowWrite": allow_write_paths,
            "denyWrite": deny_write_paths,
        }
    });
    if permission_mode.is_read_only() {
        sandbox["network"] = serde_json::json!({
            "allowedDomains": [],
            "allowLocalBinding": false,
            "allowUnixSockets": []
        });
        sandbox["allowUnsandboxedCommands"] = serde_json::json!(false);
    }
    let settings = serde_json::json!({
        "fastMode": session::claude_fast_mode(speed_mode),
        "permissions": {
            "deny": deny_rules,
        },
        "sandbox": sandbox
    });

    command.arg("--settings").arg(settings.to_string());
}

/// Returns a Claude Code permission-rule absolute path using the `//`
/// prefix required by `Read` and `Edit` path rules.
fn claude_absolute_permission_path(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let path_without_root = path.trim_start_matches('/');

    format!("//{path_without_root}")
}

#[cfg(test)]
#[path = "claude_test.rs"]
mod tests;
