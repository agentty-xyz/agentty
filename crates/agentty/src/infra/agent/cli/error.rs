//! Shared provider-aware CLI exit error formatting.

use crate::domain::agent::AgentKind;

/// Formats one failed agent CLI command into a user-facing error string.
pub(crate) fn format_agent_cli_exit_error(
    agent_kind: AgentKind,
    command_label: &str,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> String {
    if let Some(guidance) = known_agent_cli_exit_guidance(agent_kind, command_label, stdout, stderr)
    {
        return guidance;
    }

    let exit_code = exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
    let output_detail = agent_cli_output_detail(stdout, stderr);

    format!("{command_label} failed with exit code {exit_code}: {output_detail}")
}

/// Returns provider-specific guidance for known CLI command failures.
fn known_agent_cli_exit_guidance(
    agent_kind: AgentKind,
    command_label: &str,
    stdout: &str,
    stderr: &str,
) -> Option<String> {
    match agent_kind {
        AgentKind::Claude if is_claude_authentication_error(stdout, stderr) => {
            Some(claude_authentication_error_message(command_label))
        }
        AgentKind::Claude | AgentKind::Codex | AgentKind::Gemini => None,
    }
}

/// Builds the actionable Claude authentication refresh guidance message.
fn claude_authentication_error_message(command_label: &str) -> String {
    format!(
        "{command_label} failed because Claude authentication expired or is missing.\nRun `claude \
         auth login` to refresh your Anthropic session, verify with `claude auth status`, then \
         retry."
    )
}

/// Detects Claude CLI authentication failures surfaced through stdout/stderr.
fn is_claude_authentication_error(stdout: &str, stderr: &str) -> bool {
    let combined_output = format!("{stdout}\n{stderr}").to_ascii_lowercase();

    combined_output.contains("oauth token has expired")
        || combined_output.contains("failed to authenticate")
        || combined_output.contains("authentication_error")
}

/// Formats captured stdout/stderr into one compact CLI error detail string.
fn agent_cli_output_detail(stdout: &str, stderr: &str) -> String {
    let trimmed_stdout = stdout.trim();
    let trimmed_stderr = stderr.trim();

    match (trimmed_stdout.is_empty(), trimmed_stderr.is_empty()) {
        (false, false) => format!("stdout: {trimmed_stdout}; stderr: {trimmed_stderr}"),
        (false, true) => format!("stdout: {trimmed_stdout}"),
        (true, false) => format!("stderr: {trimmed_stderr}"),
        (true, true) => "no output".to_string(),
    }
}
