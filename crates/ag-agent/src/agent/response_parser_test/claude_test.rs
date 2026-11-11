use crate::agent::response_parser::{
    parse_claude_response_with_fallback, parse_claude_stream_output_line,
    parse_claude_stream_reasoning_line,
};

#[test]
fn test_claude_parse_response_reads_result_payload() {
    // Arrange
    let stdout = r#"{"result":"Planned response","usage":{"input_tokens":11,"output_tokens":7}}"#;

    // Act
    let parsed = parse_claude_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Planned response");
    assert_eq!(parsed.stats.input_tokens, 11);
    assert_eq!(parsed.stats.output_tokens, 7);
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
    assert_eq!(parsed.stats.input_tokens, 13);
    assert_eq!(parsed.stats.output_tokens, 9);
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
    assert_eq!(parsed.stats.input_tokens, 11);
    assert_eq!(parsed.stats.output_tokens, 7);
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
    assert_eq!(parsed.stats.input_tokens, 13);
    assert_eq!(parsed.stats.output_tokens, 5);
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
    assert_eq!(parsed.stats.input_tokens, 5);
    assert_eq!(parsed.stats.output_tokens, 3);
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
    assert_eq!(parsed.stats.input_tokens, 7);
    assert_eq!(parsed.stats.output_tokens, 4);
}

#[test]
fn test_parse_claude_stream_output_line_reads_assistant_message_content_blocks() {
    // Arrange
    let stdout_line = concat!(
        r#"{"type":"assistant","message":{"role":"assistant","content":["#,
        r#"{"type":"text","text":"Partial "},{"type":"text","text":"answer"}]}}"#
    );

    // Act
    let parsed_line = parse_claude_stream_output_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, Some(("Partial answer".to_string(), true)));
}

#[test]
fn test_parse_claude_stream_output_line_reads_message_event_content() {
    // Arrange
    let stdout_line = r#"{"type":"message","role":"assistant","content":"Partial answer"}"#;

    // Act
    let parsed_line = parse_claude_stream_output_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, Some(("Partial answer".to_string(), true)));
}

#[test]
fn test_parse_claude_stream_output_line_ignores_result_events_for_delta_streaming() {
    // Arrange
    let stdout_line = r#"{"type":"result","subtype":"success","result":"Final answer"}"#;

    // Act
    let parsed_line = parse_claude_stream_output_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, None);
}

#[test]
fn test_parse_claude_stream_output_line_maps_tool_use_blocks_to_progress() {
    // Arrange
    let stdout_line = concat!(
        r#"{"type":"assistant","message":{"role":"assistant","content":["#,
        r#"{"type":"tool_use","name":"Bash"}]}}"#
    );

    // Act
    let parsed_line = parse_claude_stream_output_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, Some(("Running a command".to_string(), false)));
}

#[test]
fn test_parse_claude_stream_reasoning_line_reads_thinking_blocks() {
    // Arrange
    let stdout_line = concat!(
        r#"{"type":"assistant","message":{"role":"assistant","content":["#,
        r#"{"type":"thinking","thinking":"First idea.","signature":"a"},"#,
        r#"{"type":"text","text":"answer"},"#,
        r#"{"type":"thinking","thinking":" \n","signature":"b"},"#,
        r#"{"type":"thinking","thinking":"Second idea.","signature":"c"}]}}"#
    );

    // Act
    let reasoning = parse_claude_stream_reasoning_line(stdout_line);

    // Assert
    assert_eq!(reasoning.as_deref(), Some("First idea.\nSecond idea.\n"));
}

#[test]
fn test_parse_claude_stream_reasoning_line_ignores_lines_without_reasoning() {
    // Arrange
    let lines = [
        "not json",
        r#"{"type":"result","result":"done"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":"plain"}}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"answer"}]}}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"a"}]}}"#,
    ];

    // Act
    let reasoning = lines.map(parse_claude_stream_reasoning_line);

    // Assert
    assert_eq!(reasoning, [None, None, None, None, None]);
}
