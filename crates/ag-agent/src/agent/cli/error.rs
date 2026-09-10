//! Shared provider-aware CLI exit error formatting.

use serde_json::Value;

use crate::model::agent::AgentKind;

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
        AgentKind::Antigravity | AgentKind::Claude | AgentKind::Codex | AgentKind::Gemini => None,
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

/// Maximum stream lines kept in one CLI exit error section.
///
/// Turn errors are appended to the session transcript, so an unbounded stream
/// section paints a screenful of provider event lines into the chat. The tail
/// carries the failure, so the newest lines are the ones worth keeping.
const CLI_STREAM_DETAIL_MAX_LINES: usize = 12;

/// Formats one captured stream, summarizing JSONL provider event lines while
/// preserving plain-text provider diagnostics.
///
/// Only the last [`CLI_STREAM_DETAIL_MAX_LINES`] lines are kept, prefixed with
/// a note when earlier lines were dropped.
fn format_cli_stream_output(output: &str) -> String {
    let formatted_output =
        format_json_line_stream(output).unwrap_or_else(|| output.trim_end().to_string());

    truncate_stream_detail_to_tail(&formatted_output)
}

/// Keeps only the trailing stream lines that fit the error-detail budget.
fn truncate_stream_detail_to_tail(formatted_output: &str) -> String {
    let lines: Vec<&str> = formatted_output.lines().collect();
    let dropped_line_count = lines.len().saturating_sub(CLI_STREAM_DETAIL_MAX_LINES);
    if dropped_line_count == 0 {
        return formatted_output.to_string();
    }

    let tail = lines[dropped_line_count..].join("\n");

    format!("[{dropped_line_count} earlier lines omitted]\n{tail}")
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
#[path = "error_test.rs"]
mod tests;
