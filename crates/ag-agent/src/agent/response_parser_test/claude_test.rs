//! Claude response and stream-line parsing unit tests.

use ag_contracts::{ContextFill, ModelUsage};

use crate::agent::cli::execution::CliStdoutPayload;
use crate::agent::response_parser::{
    parse_claude_response_with_fallback, parse_claude_stream_payload,
};

/// Classifies one stdout line through the shared CLI payload parse.
fn parse_stream_line(line: &str) -> Option<(String, bool)> {
    CliStdoutPayload::parse(line)
        .trimmed()
        .and_then(parse_claude_stream_payload)
}

#[test]
fn test_claude_parse_response_reads_result_payload() {
    // Arrange
    let stdout = r#"{"result":"Planned response","usage":{"input_tokens":11,"output_tokens":7}}"#;

    // Act
    let parsed = parse_claude_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Planned response");
    assert_eq!(parsed.usage.input_tokens, 11);
    assert_eq!(parsed.usage.output_tokens, 7);
}

#[test]
fn test_claude_parse_response_reads_structured_output_payload() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"result","subtype":"success","result":"","structured_output":"#,
        r#"{"answer":"Rendered response","questions":[]},"#,
        r#""usage":{"input_tokens":13,"output_tokens":9}}"#
    );

    // Act
    let parsed = parse_claude_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(
        parsed.content,
        r#"{"answer":"Rendered response","questions":[]}"#
    );
    assert_eq!(parsed.usage.input_tokens, 13);
    assert_eq!(parsed.usage.output_tokens, 9);
}

#[test]
fn test_claude_parse_response_uses_stream_message_when_result_is_empty() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"message","role":"assistant","content":"Recovered response payload"}"#,
        "\n",
        r#"{"type":"result","subtype":"success","result":"","usage":{"input_tokens":11,"output_tokens":7}}"#
    );

    // Act
    let parsed = parse_claude_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Recovered response payload");
    assert_eq!(parsed.usage.input_tokens, 11);
    assert_eq!(parsed.usage.output_tokens, 7);
}

#[test]
fn test_claude_parse_response_reads_usage_when_result_is_missing() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"message","role":"assistant","content":"Recovered response payload"}"#,
        "\n",
        r#"{"type":"result","subtype":"success","usage":{"input_tokens":13,"output_tokens":5}}"#
    );

    // Act
    let parsed = parse_claude_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Recovered response payload");
    assert_eq!(parsed.usage.input_tokens, 13);
    assert_eq!(parsed.usage.output_tokens, 5);
}

#[test]
fn test_claude_parse_response_keeps_follow_up_turn_results_and_sums_usage() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"result","result":"First answer","usage":{"input_tokens":11,"output_tokens":7}}"#,
        "\n",
        r#"{"type":"result","subtype":"error_during_execution","usage":{"input_tokens":2,"output_tokens":1}}"#,
        "\n",
        r#"{"type":"result","result":"Steer answer","usage":{"input_tokens":5,"output_tokens":3}}"#
    );

    // Act
    let parsed = parse_claude_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "First answer");
    assert_eq!(parsed.follow_up_contents, vec!["Steer answer".to_string()]);
    assert_eq!(parsed.usage.input_tokens, 18);
    assert_eq!(parsed.usage.output_tokens, 11);
}

#[test]
fn test_claude_parse_response_reads_result_from_json_array_payload() {
    // Arrange
    let stdout = r#"[{"type":"system","subtype":"init"},{"type":"result","subtype":"success","result":"{\"answer\":\"Recovered payload\",\"questions\":[]}","usage":{"input_tokens":5,"output_tokens":3}}]"#;

    // Act
    let parsed = parse_claude_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(
        parsed.content,
        r#"{"answer":"Recovered payload","questions":[]}"#
    );
    assert_eq!(parsed.usage.input_tokens, 5);
    assert_eq!(parsed.usage.output_tokens, 3);
}

#[test]
fn test_claude_parse_response_reads_structured_output_from_json_array_payload() {
    // Arrange
    let stdout = concat!(
        r#"[{"type":"system","subtype":"init"},"#,
        r#"{"type":"result","subtype":"success","result":"","structured_output":"#,
        r#"{"answer":"Array response","questions":[]},"#,
        r#""usage":{"input_tokens":7,"output_tokens":4}}]"#
    );

    // Act
    let parsed = parse_claude_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(
        parsed.content,
        r#"{"answer":"Array response","questions":[]}"#
    );
    assert_eq!(parsed.usage.input_tokens, 7);
    assert_eq!(parsed.usage.output_tokens, 4);
}

#[test]
fn test_parse_claude_stream_output_line_reads_assistant_message_content_blocks() {
    // Arrange
    let stdout_line = concat!(
        r#"{"type":"assistant","message":{"role":"assistant","content":["#,
        r#"{"type":"text","text":"Partial "},{"type":"text","text":"answer"}]}}"#
    );

    // Act
    let parsed_line = parse_stream_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, Some(("Partial answer".to_string(), true)));
}

#[test]
fn test_parse_claude_stream_output_line_reads_message_event_content() {
    // Arrange
    let stdout_line = r#"{"type":"message","role":"assistant","content":"Partial answer"}"#;

    // Act
    let parsed_line = parse_stream_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, Some(("Partial answer".to_string(), true)));
}

#[test]
fn test_parse_claude_stream_output_line_ignores_result_events_for_delta_streaming() {
    // Arrange
    let stdout_line = r#"{"type":"result","subtype":"success","result":"Final answer"}"#;

    // Act
    let parsed_line = parse_stream_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, None);
}

#[test]
fn test_claude_parse_response_recovers_stream_text_and_falls_back_without_it() {
    // Arrange
    let whitespace_blocks = concat!(
        r#"{"type":"assistant","message":{"role":"assistant","content":["#,
        r#"{"type":"text","text":"  "}]}}"#
    );
    let whitespace_text = r#"{"type":"message","role":"assistant","content":"   "}"#;
    let response_cases = [
        ("", "auth failed", "auth failed".to_string()),
        (
            "\n{\"type\":\"message\",\"role\":\"assistant\",\"content\":\"Hi\"}\n\n",
            "",
            "Hi".to_string(),
        ),
        (
            concat!(
                r#"[{"type":"assistant","message":{"role":"assistant","content":["#,
                r#"{"type":"text_delta","delta":{"text":"Array reply"}}]}}]"#
            ),
            "",
            "Array reply".to_string(),
        ),
        (whitespace_blocks, "", whitespace_blocks.to_string()),
        (whitespace_text, "", whitespace_text.to_string()),
    ];

    for (stdout, stderr, expected_content) in response_cases {
        // Act
        let parsed = parse_claude_response_with_fallback(stdout, stderr);

        // Assert
        assert_eq!(parsed.content, expected_content, "stdout: {stdout:?}");
        assert_eq!(parsed.usage, ModelUsage::unreported("", 1));
        assert_eq!(parsed.reported_cost, None);
    }
}

#[test]
fn test_claude_parse_response_normalizes_cache_dedups_messages_and_reads_model_usage() {
    // Arrange
    let first_response = concat!(
        r#"{"type":"assistant","message":{"id":"msg-1","model":"claude-opus-5-5","content":[],"#,
        r#""usage":{"input_tokens":5,"cache_read_input_tokens":1000,"cache_creation_input_tokens":10,"output_tokens":1}}}"#
    );
    let stdout = [
        first_response,
        first_response,
        concat!(
            r#"{"type":"assistant","message":{"id":"msg-2","model":"claude-opus-5-5","content":[],"#,
            r#""usage":{"input_tokens":20,"cache_read_input_tokens":1015,"cache_creation_input_tokens":0,"output_tokens":1}}}"#
        ),
        concat!(
            r#"{"type":"result","result":"Done","#,
            r#""usage":{"input_tokens":25,"cache_read_input_tokens":2015,"cache_creation_input_tokens":10,"output_tokens":50},"#,
            r#""modelUsage":{"claude-opus-5-5":{"contextWindow":200000,"costUSD":0.0125}}}"#
        ),
    ]
    .join("\n");

    // Act
    let parsed = parse_claude_response_with_fallback(&stdout, "");

    // Assert
    assert_eq!(parsed.content, "Done");
    assert_eq!(parsed.reported_cost, Some(12_500));
    assert_eq!(
        parsed.usage,
        ModelUsage {
            cache_read_tokens: Some(2_015),
            cache_write_tokens: Some(10),
            context: Some(ContextFill {
                last_input_tokens: 1_035,
                peak_input_tokens: 1_035,
                window_tokens: Some(200_000),
            }),
            input_tokens: 2_050,
            output_tokens: 50,
            requests: 2,
            ..ModelUsage::new("")
        }
    );
}

#[test]
fn test_parse_claude_stream_output_line_maps_progress_labels_from_each_field() {
    // Arrange
    let line_cases = [
        ("  ", None),
        (
            r#"{"type":"x","item":{"type":"web_search"}}"#,
            Some("Searching the web"),
        ),
        (r#"{"name":"Bash"}"#, Some("Running a command")),
        (
            r#"{"tool":{"name":"WebSearch"}}"#,
            Some("Searching the web"),
        ),
        (r#"{"event":"thinking"}"#, Some("Thinking")),
        (
            r#"{"subtype":"compact_boundary"}"#,
            Some("Compacting context"),
        ),
        (r#"{"type":"tool_call"}"#, Some("Running a command")),
    ];

    for (stdout_line, expected_progress) in line_cases {
        // Act
        let parsed_line = parse_stream_line(stdout_line);

        // Assert
        assert_eq!(
            parsed_line,
            expected_progress.map(|progress| (progress.to_string(), false)),
            "line: {stdout_line:?}"
        );
    }
}

#[test]
fn test_parse_claude_stream_output_line_maps_tool_use_blocks_to_progress() {
    // Arrange
    let stdout_line = concat!(
        r#"{"type":"assistant","message":{"role":"assistant","content":["#,
        r#"{"type":"tool_use","name":"Bash"}]}}"#
    );

    // Act
    let parsed_line = parse_stream_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, Some(("Running a command".to_string(), false)));
}
