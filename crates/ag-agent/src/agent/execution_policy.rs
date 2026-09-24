use ag_contracts::{ExecutionPolicy, McpPolicy, PermissionMode, ToolPolicy};
use ag_session::AgentKind;

use super::backend::AgentBackendError;

/// Rejects requested controls that the selected adapter cannot enforce.
pub(super) fn validate(kind: AgentKind, policy: &ExecutionPolicy) -> Result<(), AgentBackendError> {
    let unsupported = if policy.max_concurrent_subagents.is_some()
        && !matches!(kind, AgentKind::Claude | AgentKind::Codex)
    {
        Some("subagent concurrency")
    } else if policy.tools != ToolPolicy::Inherit && kind != AgentKind::Claude {
        Some("built-in tool policy")
    } else if policy.mcp != McpPolicy::Inherit && kind != AgentKind::Claude {
        Some("MCP policy")
    } else {
        None
    };
    if let Some(control) = unsupported {
        return Err(AgentBackendError::CommandBuild(format!(
            "{kind} does not support the requested {control}"
        )));
    }

    Ok(())
}

/// Translates Claude tool controls without weakening research permissions.
pub(super) fn apply_claude_tools(
    command: &mut std::process::Command,
    policy: &ToolPolicy,
    permission_mode: PermissionMode,
) {
    const READ_ONLY: &[&str] = &["Read", "Glob", "Grep", "WebSearch", "WebFetch"];
    if permission_mode.is_read_only() {
        let tools = match policy {
            ToolPolicy::AllowOnly(tools) => tools
                .iter()
                .filter(|tool| READ_ONLY.contains(&tool.as_str()))
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
            ToolPolicy::AutoApprove(_) | ToolPolicy::Inherit => READ_ONLY.join(","),
        };
        command
            .arg("--tools")
            .arg(&tools)
            .arg("--allowedTools")
            .arg(tools)
            .arg("--permission-mode")
            .arg("plan");
        return;
    }
    match policy {
        ToolPolicy::AllowOnly(tools) => {
            let tools = tools.join(",");
            command
                .arg("--tools")
                .arg(&tools)
                .arg("--allowedTools")
                .arg(tools);
        }
        ToolPolicy::AutoApprove(tools) => {
            command.arg("--allowedTools").arg(tools.join(","));
        }
        ToolPolicy::Inherit => {}
    }
}

#[cfg(test)]
#[path = "execution_policy_test.rs"]
mod tests;
