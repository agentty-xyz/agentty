use crate::agent::cli::error::{
    agent_cli_output_detail, format_agent_cli_exit_error, format_cli_stream_output,
    is_claude_authentication_error, known_agent_cli_exit_guidance,
};
use crate::model::agent::AgentKind;

#[test]
/// A long provider stream is reduced to its tail so a failing turn cannot
/// paint every event line into the session transcript.
fn test_format_agent_cli_exit_error_bounds_long_stream_detail() {
    // Arrange
    let stdout = (0..40)
        .map(|index| format!(r#"{{"type":"system","subtype":"event_{index}"}}"#))
        .collect::<Vec<_>>()
        .join("\n");

    // Act
    let error =
        format_agent_cli_exit_error(AgentKind::Claude, "Agent command", Some(1), &stdout, "");

    // Assert
    assert!(error.contains("earlier lines omitted"));
    assert!(!error.contains("system | event_0"));
    assert!(error.contains("system | event_39"));
}

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
    assert!(
        result.contains(
            "message: You've hit your session limit - resets 12:10am (America/Los_Angeles)"
        )
    );
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
