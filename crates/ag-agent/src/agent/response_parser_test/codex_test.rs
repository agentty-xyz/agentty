use crate::agent::response_parser::{
    compact_codex_progress_message, parse_codex_response_with_fallback,
    parse_codex_stream_output_line,
};

/// Ensures final NDJSON parsing keeps the real assistant reply when
/// completion-status messages trail the stream.
#[test]
fn test_parse_response_codex_ignores_trailing_completion_status_message() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Planned final answer"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":21,"output_tokens":8}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Command completed"}}"#,
    );

    // Act
    let parsed = parse_codex_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Planned final answer");
    assert_eq!(parsed.stats.input_tokens, 21);
    assert_eq!(parsed.stats.output_tokens, 8);
}

#[test]
/// Ensures trailing reasoning payloads do not overwrite final
/// `agent_message` output.
fn test_parse_response_codex_prefers_agent_message_over_trailing_reasoning_payload() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Focused review summary"}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"type":"reasoning","text":"{\"title\":\"Inject clock dependency to enforce time boundaries\"}"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":13,"output_tokens":9}}"#,
    );

    // Act
    let parsed = parse_codex_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Focused review summary");
    assert_eq!(parsed.stats.input_tokens, 13);
    assert_eq!(parsed.stats.output_tokens, 9);
}

#[test]
/// Ensures parsing still returns reasoning text when no
/// `agent_message` item exists.
fn test_parse_response_codex_falls_back_to_reasoning_without_agent_message() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"item.completed","item":{"type":"reasoning","text":"Fallback reasoning output"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":5,"output_tokens":3}}"#,
    );

    // Act
    let parsed = parse_codex_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Fallback reasoning output");
    assert_eq!(parsed.stats.input_tokens, 5);
    assert_eq!(parsed.stats.output_tokens, 3);
}

#[test]
fn test_parse_response_codex_ignores_completion_status_as_final_message() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Final answer"}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Command completed"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":13,"output_tokens":9}}"#,
    );

    // Act
    let parsed = parse_codex_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Final answer");
    assert_eq!(parsed.stats.input_tokens, 13);
    assert_eq!(parsed.stats.output_tokens, 9);
}

#[test]
fn test_parse_stream_output_line_codex_ignores_completion_status_messages() {
    // Arrange
    let completion_status_lines = [
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Command completed"}}"#,
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Thinking completed"}}"#,
    ];

    // Act & Assert
    for completion_status_line in completion_status_lines {
        let parsed_line = parse_codex_stream_output_line(completion_status_line);

        assert_eq!(parsed_line, None);
    }
}

#[test]
fn test_parse_stream_output_line_codex_keeps_assistant_message_content() {
    // Arrange
    let assistant_line =
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Final answer"}}"#;

    // Act
    let parsed_line = parse_codex_stream_output_line(assistant_line);

    // Assert
    assert_eq!(parsed_line, Some(("Final answer".to_string(), true)));
}

#[test]
fn test_parse_stream_output_line_codex_marks_reasoning_as_loader_text() {
    // Arrange
    let reasoning_line =
        r#"{"type":"item.updated","item":{"type":"reasoning","delta":"Inspecting files"}}"#;

    // Act
    let parsed_line = parse_codex_stream_output_line(reasoning_line);

    // Assert
    assert_eq!(parsed_line, Some(("Inspecting files".to_string(), false)));
}

#[test]
fn test_parse_stream_output_line_codex_classifies_each_event_shape() {
    // Arrange
    let line_cases = [
        ("   ", None),
        (r#"{"type":"item.started"}"#, None),
        (
            r#"{"type":"item.started","item":{"type":"thought"}}"#,
            Some(("Thinking".to_string(), false)),
        ),
        (
            r#"{"type":"item.updated","item":{"type":"thought","text":"Weighing options"}}"#,
            Some(("Weighing options".to_string(), false)),
        ),
        (
            r#"{"type":"item.updated","item":{"type":"agentmessage","delta":"Partial"}}"#,
            Some(("Partial".to_string(), true)),
        ),
        (
            r#"{"type":"item.updated","item":{"type":"command_execution","text":"ls"}}"#,
            None,
        ),
        (
            r#"{"type":"item.completed","item":{"type":"agentmessage","text":"Done"}}"#,
            Some(("Done".to_string(), true)),
        ),
        (
            r#"{"type":"item.completed","item":{"type":"command_execution","text":"ls"}}"#,
            None,
        ),
    ];

    for (line, expected) in line_cases {
        // Act
        let parsed_line = parse_codex_stream_output_line(line);

        // Assert
        assert_eq!(parsed_line, expected, "line: {line}");
    }
}

#[test]
fn test_parse_response_codex_skips_unusable_events() {
    // Arrange
    let stdout = concat!(
        "not json\n",
        "\n",
        r#"{"type":"turn.completed"}"#,
        "\n",
        r#"{"type":"item.completed"}"#,
        "\n",
        r#"{"type":"item.completed","item":{"type":"agent_message"}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"type":"command_execution","text":"ls"}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"type":"agent_message","delta":"Delta answer"}}"#,
    );

    // Act
    let parsed = parse_codex_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Delta answer");
    assert_eq!(parsed.stats.input_tokens, 0);
    assert_eq!(parsed.stats.output_tokens, 0);
}

#[test]
fn compact_codex_progress_message_returns_compacting_for_context_compaction() {
    // Arrange

    // Act
    let progress = compact_codex_progress_message("context_compaction");

    // Assert
    assert_eq!(progress, Some("Compacting context".to_string()));
}

#[test]
fn compact_codex_progress_message_maps_search_and_unknown_item_types() {
    // Arrange
    let item_cases = [
        ("agent_message", None),
        ("web_search", Some("Searching the web")),
        ("file_change", Some("Working: file change")),
    ];

    for (item_type, expected_progress) in item_cases {
        // Act
        let progress = compact_codex_progress_message(item_type);

        // Assert
        assert_eq!(
            progress,
            expected_progress.map(ToString::to_string),
            "item type `{item_type}`"
        );
    }
}
