use ag_contracts::SessionDiffState;

use crate::agent::response_parser::{
    parse_gemini_response_with_fallback, parse_gemini_stream_output_line,
};

#[test]
fn test_gemini_parse_response_reads_legacy_usage() {
    // Arrange
    let stdout = r#"{"response":"Planned response","stats":{"models":{"gemini":{"tokens":{"input":11,"candidates":7}}}}}"#;

    // Act
    let parsed = parse_gemini_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Planned response");
    assert_eq!(parsed.stats.input_tokens, 11);
    assert_eq!(parsed.stats.output_tokens, 7);
    assert_eq!(parsed.stats.diff_state, SessionDiffState::Unknown);
}

#[test]
fn test_gemini_parse_response_reads_stream_usage() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"content","text":"Streamed response"}"#,
        "\n",
        r#"{"type":"result","stats":{"input_tokens":13,"output_tokens":5}}"#
    );

    // Act
    let parsed = parse_gemini_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Streamed response");
    assert_eq!(parsed.stats.input_tokens, 13);
    assert_eq!(parsed.stats.output_tokens, 5);
    assert_eq!(parsed.stats.diff_state, SessionDiffState::Unknown);
}

#[test]
fn test_gemini_parse_response_handles_mixed_and_unusable_output() {
    // Arrange
    let response_cases = [
        (
            concat!(
                "warning: cached credentials\n",
                "\n",
                r#"{"type":"init"}"#,
                "\n",
                r#"{"response":"Legacy line response"}"#
            ),
            "",
            "Legacy line response",
        ),
        ("not json output\n", "fatal: no model", "not json output"),
        ("", "fatal: no model", "fatal: no model"),
    ];

    for (stdout, stderr, expected_content) in response_cases {
        // Act
        let parsed = parse_gemini_response_with_fallback(stdout, stderr);

        // Assert
        assert_eq!(parsed.content, expected_content, "stdout: {stdout:?}");
        assert_eq!(parsed.stats.input_tokens, 0);
        assert_eq!(parsed.stats.output_tokens, 0);
    }
}

#[test]
fn test_parse_gemini_stream_output_line_classifies_content_and_progress() {
    // Arrange
    let line_cases = [
        ("  ", None),
        ("not json", None),
        (
            r#"{"response":"Direct chunk"}"#,
            Some(("Direct chunk".to_string(), true)),
        ),
        (
            r#"{"response":"","type":"content","text":"Text chunk"}"#,
            Some(("Text chunk".to_string(), true)),
        ),
        (
            r#"{"type":"delta","delta":"Delta chunk"}"#,
            Some(("Delta chunk".to_string(), true)),
        ),
        (
            r#"{"type":"message","content":"Message chunk"}"#,
            Some(("Message chunk".to_string(), true)),
        ),
        (
            r#"{"type":"tool_use","tool_name":"run_shell_command"}"#,
            Some(("Running a command".to_string(), false)),
        ),
        (r#"{"type":"init"}"#, None),
    ];

    for (line, expected) in line_cases {
        // Act
        let parsed = parse_gemini_stream_output_line(line);

        // Assert
        assert_eq!(parsed, expected, "line: {line:?}");
    }
}
