//! Shared provider-aware CLI exit error formatting.

use serde_json::Value;

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

    format!("{command_label} failed with exit code {exit_code}.\n\n{output_detail}")
}

/// Returns provider-specific guidance for known CLI command failures.
fn known_agent_cli_exit_guidance(
    agent_kind: AgentKind,
    command_label: &str,
    stdout: &str,
    stderr: &str,
) -> Option<String> {
    match agent_kind {
        AgentKind::Antigravity if is_antigravity_authentication_error(stdout, stderr) => {
            Some(antigravity_authentication_error_message(command_label))
        }
        AgentKind::Claude if is_claude_authentication_error(stdout, stderr) => {
            Some(claude_authentication_error_message(command_label))
        }
        AgentKind::Antigravity | AgentKind::Claude | AgentKind::Codex => None,
    }
}

/// Builds actionable Antigravity authentication refresh guidance.
fn antigravity_authentication_error_message(command_label: &str) -> String {
    format!(
        "{command_label} failed because Antigravity authentication is missing.\nRun `agy` in a \
         terminal to complete Google sign-in, then retry."
    )
}

/// Builds the actionable Claude authentication refresh guidance message.
fn claude_authentication_error_message(command_label: &str) -> String {
    format!(
        "{command_label} failed because Claude authentication expired or is missing.\nRun `claude \
         auth login` to refresh your Anthropic session, verify with `claude auth status`, then \
         retry."
    )
}

/// Detects Antigravity CLI authentication failures surfaced through
/// stdout/stderr.
fn is_antigravity_authentication_error(stdout: &str, stderr: &str) -> bool {
    let combined_output = format!("{stdout}\n{stderr}").to_ascii_lowercase();

    combined_output.contains("you are not logged into antigravity")
        || combined_output.contains("failed to get oauth token")
        || combined_output.contains("error getting token source")
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
        (false, false) => format_cli_stream_detail(
            &[
                cli_stream_section("stdout", trimmed_stdout),
                cli_stream_section("stderr", trimmed_stderr),
            ]
            .join("\n\n"),
        ),
        (false, true) => format_cli_stream_detail(&cli_stream_section("stdout", trimmed_stdout)),
        (true, false) => format_cli_stream_detail(&cli_stream_section("stderr", trimmed_stderr)),
        (true, true) => "no output".to_string(),
    }
}

/// Wraps one stream section in a readable error block with a short label.
fn cli_stream_section(label: &str, output: &str) -> String {
    let formatted_output = format_cli_stream_output(output);

    format!("{label}:\n```text\n{formatted_output}\n```")
}

/// Formats one captured stream, summarizing JSONL provider event lines while
/// preserving plain-text provider diagnostics.
fn format_cli_stream_output(output: &str) -> String {
    format_json_line_stream(output).unwrap_or_else(|| output.to_string())
}

/// Converts each JSON object line in a provider stream into readable text.
fn format_json_line_stream(output: &str) -> Option<String> {
    let mut has_json_object = false;
    let formatted_lines = output
        .lines()
        .map(|line| {
            let trimmed_line = line.trim();
            if trimmed_line.is_empty() {
                return line.to_string();
            }

            match serde_json::from_str::<Value>(trimmed_line) {
                Ok(value @ Value::Object(_)) => {
                    has_json_object = true;

                    format_json_event_summary(&value)
                }
                Ok(_) | Err(_) => line.to_string(),
            }
        })
        .collect::<Vec<_>>();

    if !has_json_object {
        return None;
    }

    Some(formatted_lines.join("\n"))
}

/// Builds a compact human-readable summary for one provider JSON event.
fn format_json_event_summary(value: &Value) -> String {
    match json_string(value, "type").as_deref() {
        Some("assistant") => format_assistant_event_summary(value),
        Some("result") => format_result_event_summary(value),
        Some("system") => format_system_event_summary(value),
        Some(event_type) => format_generic_event_summary(value, event_type),
        None => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

/// Summarizes one assistant-message event without dumping provider metadata.
fn format_assistant_event_summary(value: &Value) -> String {
    let message = value
        .get("message")
        .and_then(assistant_message_text)
        .or_else(|| assistant_message_text(value));

    message.map_or_else(
        || format_generic_event_summary(value, "assistant"),
        |message| format!("assistant: {message}"),
    )
}

/// Summarizes one terminal result event, prioritizing error and request
/// details because those are the actionable fields for failed commands.
fn format_result_event_summary(value: &Value) -> String {
    let mut lines = Vec::new();
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let result_label = json_string(value, "error").map_or_else(
        || "result".to_string(),
        |error| format!("result error: {error}"),
    );

    lines.push(result_label);
    if let Some(result) = json_string(value, "result")
        && !result.is_empty()
    {
        lines.push(format!("message: {result}"));
    }
    if let Some(api_error_status) = json_scalar(value, "api_error_status") {
        lines.push(format!("api error status: {api_error_status}"));
    }
    if let Some(request_id) = json_string(value, "request_id") {
        lines.push(format!("request id: {request_id}"));
    }
    if let Some(duration_ms) = value.get("duration_ms").and_then(Value::as_u64) {
        lines.push(format!("duration: {duration_ms}ms"));
    }
    if is_error && lines.len() == 1 {
        lines[0].push_str(" (failed)");
    }

    lines.join("\n")
}

/// Summarizes one system event while keeping path and session metadata short.
fn format_system_event_summary(value: &Value) -> String {
    let mut parts = vec!["system".to_string()];
    if let Some(subtype) = json_string(value, "subtype") {
        parts.push(subtype);
    }
    if let Some(cwd) = json_string(value, "cwd") {
        parts.push(format!("cwd: {cwd}"));
    }
    if let Some(session_id) = json_string(value, "session_id") {
        parts.push(format!("session: {session_id}"));
    }

    parts.join(" | ")
}

/// Summarizes unknown provider event objects by listing common scalar fields.
fn format_generic_event_summary(value: &Value, event_type: &str) -> String {
    let mut parts = vec![event_type.to_string()];

    for field in [
        "subtype",
        "status",
        "reason",
        "error",
        "api_error_status",
        "request_id",
        "session_id",
    ] {
        if let Some(field_value) = json_scalar(value, field) {
            parts.push(format!("{field}: {field_value}"));
        }
    }

    if let Some(rate_limit_info) = value.get("rate_limit_info").and_then(Value::as_object) {
        if let Some(status) = rate_limit_info.get("status").and_then(Value::as_str) {
            parts.push(format!("rate_limit_status: {status}"));
        }
        if let Some(reason) = rate_limit_info.get("reason").and_then(Value::as_str) {
            parts.push(format!("rate_limit_reason: {reason}"));
        }
    }

    parts.join(" | ")
}

/// Extracts assistant text from either a simple string field or a standard
/// provider content array.
fn assistant_message_text(value: &Value) -> Option<String> {
    if let Some(content) = json_string(value, "content") {
        return Some(content);
    }

    let text = value
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return None;
    }

    Some(text)
}

/// Returns one string field from a JSON object.
fn json_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Returns one scalar field from a JSON object as display text.
fn json_scalar(value: &Value, field: &str) -> Option<String> {
    match value.get(field)? {
        Value::Bool(bool_value) => Some(bool_value.to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::String(string) => Some(string.clone()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Trims a trailing newline so error message comparisons and rendering stay
/// stable when callers append additional context.
fn format_cli_stream_detail(detail: &str) -> String {
    detail.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- format_agent_cli_exit_error ---

    #[test]
    fn test_format_generic_error_with_exit_code_and_stderr() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude code",
            Some(1),
            "",
            "something broke",
        );

        // Assert
        assert!(result.contains("claude code failed with exit code 1"));
        assert!(result.contains("stderr:\n```text\nsomething broke\n```"));
    }

    #[test]
    fn test_format_generic_error_with_unknown_exit_code() {
        // Arrange / Act
        let result =
            format_agent_cli_exit_error(AgentKind::Antigravity, "agy", None, "", "crash output");

        // Assert
        assert!(result.contains("exit code unknown"));
    }

    #[test]
    fn test_format_generic_error_with_both_stdout_and_stderr() {
        // Arrange / Act
        let result =
            format_agent_cli_exit_error(AgentKind::Codex, "codex", Some(2), "out text", "err text");

        // Assert
        assert!(result.contains("stdout:\n```text\nout text\n```"));
        assert!(result.contains("stderr:\n```text\nerr text\n```"));
    }

    #[test]
    fn test_format_generic_error_summarizes_claude_jsonl_rate_limit() {
        // Arrange
        let stdout = [
            r#"{"type":"system","subtype":"init","cwd":"/private/tmp/test-agentty/wt/d4ab835d","session_id":"729a8ede-900d-4d2f-97e2-fef9b8a3593a"}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","reason":"out_of_credits","isUsingOverage":false}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"duration_ms":283,"duration_api_ms":0,"num_turns":1,"result":"You've hit your session limit - resets 12:10am (America/Los_Angeles)","session_id":"729a8ede-900d-4d2f-97e2-fef9b8a3593a","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0},"error":"rate_limit","request_id":"req_011Cbfc7AF16gbH"}"#,
        ]
        .join("\n");

        // Act
        let result =
            format_agent_cli_exit_error(AgentKind::Claude, "Agent command", Some(1), &stdout, "");

        // Assert
        assert!(result.contains("Agent command failed with exit code 1."));
        assert!(result.contains("system | init | cwd: /private/tmp/test-agentty/wt/d4ab835d"));
        assert!(result.contains("rate_limit_event | rate_limit_status: rejected"));
        assert!(result.contains("assistant: hi"));
        assert!(result.contains("result error: rate_limit"));
        assert!(result.contains(
            "message: You've hit your session limit - resets 12:10am (America/Los_Angeles)"
        ));
        assert!(result.contains("api error status: 429"));
        assert!(result.contains("request id: req_011Cbfc7AF16gbH"));
    }

    #[test]
    fn test_format_generic_error_with_no_output() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(AgentKind::Claude, "claude", Some(1), "", "");

        // Assert
        assert!(result.contains("no output"));
    }

    // --- Claude authentication error detection ---

    #[test]
    fn test_claude_auth_error_from_expired_oauth_token_in_stderr() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude code",
            Some(1),
            "",
            "OAuth token has expired",
        );

        // Assert
        assert!(result.contains("authentication expired or is missing"));
        assert!(result.contains("claude auth login"));
    }

    #[test]
    fn test_antigravity_auth_error_from_not_logged_in_message() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Antigravity,
            "agy",
            Some(1),
            "",
            "You are not logged into Antigravity.",
        );

        // Assert
        assert!(result.contains("Antigravity authentication is missing"));
        assert!(result.contains("Run `agy`"));
    }

    #[test]
    fn test_claude_auth_error_from_failed_to_authenticate_in_stdout() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude code",
            Some(1),
            "Failed to authenticate user",
            "",
        );

        // Assert
        assert!(result.contains("authentication expired or is missing"));
    }

    #[test]
    fn test_claude_auth_error_from_authentication_error_keyword() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude code",
            Some(1),
            "",
            "authentication_error: invalid key",
        );

        // Assert
        assert!(result.contains("claude auth login"));
    }

    #[test]
    fn test_claude_auth_detection_is_case_insensitive() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Claude,
            "claude",
            Some(1),
            "OAUTH TOKEN HAS EXPIRED",
            "",
        );

        // Assert
        assert!(result.contains("authentication expired or is missing"));
    }

    #[test]
    fn test_non_claude_provider_ignores_auth_keywords() {
        // Arrange / Act
        let result = format_agent_cli_exit_error(
            AgentKind::Antigravity,
            "agy",
            Some(1),
            "",
            "OAuth token has expired",
        );

        // Assert
        assert!(!result.contains("claude auth login"));
        assert!(result.contains("exit code 1"));
    }

    // --- agent_cli_output_detail ---

    #[test]
    fn test_output_detail_with_only_stdout() {
        // Arrange / Act
        let detail = agent_cli_output_detail("output text", "");

        // Assert
        assert_eq!(detail, "stdout:\n```text\noutput text\n```");
    }

    #[test]
    fn test_output_detail_with_only_stderr() {
        // Arrange / Act
        let detail = agent_cli_output_detail("", "error text");

        // Assert
        assert_eq!(detail, "stderr:\n```text\nerror text\n```");
    }

    #[test]
    fn test_output_detail_with_both_streams() {
        // Arrange / Act
        let detail = agent_cli_output_detail("out", "err");

        // Assert
        assert_eq!(
            detail,
            "stdout:\n```text\nout\n```\n\nstderr:\n```text\nerr\n```"
        );
    }

    #[test]
    fn test_output_detail_with_empty_streams() {
        // Arrange / Act
        let detail = agent_cli_output_detail("", "");

        // Assert
        assert_eq!(detail, "no output");
    }

    #[test]
    fn test_output_detail_trims_whitespace() {
        // Arrange / Act
        let detail = agent_cli_output_detail("  out  ", "  err  ");

        // Assert
        assert_eq!(
            detail,
            "stdout:\n```text\nout\n```\n\nstderr:\n```text\nerr\n```"
        );
    }

    #[test]
    fn test_format_cli_stream_output_preserves_non_json_text() {
        // Arrange / Act
        let output = format_cli_stream_output("plain text\nwith detail");

        // Assert
        assert_eq!(output, "plain text\nwith detail");
    }

    #[test]
    fn test_format_cli_stream_output_summarizes_json_lines_with_plain_text() {
        // Arrange
        let input = [
            "proxy warning: retrying",
            r#"{"type":"result","is_error":true,"error":"rate_limit","result":"session limit","request_id":"req_123"}"#,
            "plain tail",
        ]
        .join("\n");
        let expected = [
            "proxy warning: retrying",
            "result error: rate_limit",
            "message: session limit",
            "request id: req_123",
            "plain tail",
        ]
        .join("\n");

        // Act
        let output = format_cli_stream_output(&input);

        // Assert
        assert_eq!(output, expected);
    }

    // --- is_claude_authentication_error ---

    #[test]
    fn test_is_claude_auth_error_returns_false_for_unrelated_output() {
        // Arrange / Act / Assert
        assert!(!is_claude_authentication_error(
            "normal output",
            "normal error"
        ));
    }

    #[test]
    fn test_is_claude_auth_error_returns_true_for_mixed_case_in_stderr() {
        // Arrange / Act / Assert
        assert!(is_claude_authentication_error("", "FAILED TO AUTHENTICATE"));
    }

    // --- known_agent_cli_exit_guidance ---

    #[test]
    fn test_known_guidance_returns_none_for_non_auth_claude_error() {
        // Arrange / Act
        let result =
            known_agent_cli_exit_guidance(AgentKind::Claude, "claude", "normal out", "normal err");

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn test_known_guidance_returns_none_for_codex() {
        // Arrange / Act
        let result =
            known_agent_cli_exit_guidance(AgentKind::Codex, "codex", "OAuth token has expired", "");

        // Assert
        assert!(result.is_none());
    }
}
