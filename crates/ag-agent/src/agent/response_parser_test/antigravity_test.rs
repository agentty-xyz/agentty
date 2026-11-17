//! Antigravity response and stream-line parsing unit tests.

use ag_contracts::{SessionDiffState, SessionStats};
use ag_protocol::{FocusedReview, ReviewMetadata, UtilityResponse};

use crate::agent::cli::execution::CliStdoutPayload;
use crate::agent::response_parser::{
    antigravity_protocol_payload, antigravity_value_text, parse_antigravity_response_with_fallback,
    parse_antigravity_stream_payload,
};

/// Classifies one stdout line through the shared CLI payload parse.
fn parse_stream_line(line: &str) -> Option<(String, bool)> {
    CliStdoutPayload::parse(line)
        .trimmed()
        .and_then(parse_antigravity_stream_payload)
}

#[test]
/// Ensures the supported Antigravity final stream envelope exposes its
/// nested schema-constrained response and usage.
fn test_antigravity_parse_response_reads_stream_result() {
    // Arrange
    let response = serde_json::json!({
        "answer": "Antigravity ok",
        "questions": [],
        "review_comment_outcomes": [],
    });
    let stdout = serde_json::json!({
        "event": "result",
        "result": {
            "conversation_id": "conversation-1",
            "status": "SUCCESS",
            "response": response.to_string(),
            "error": "",
            "duration_seconds": 1.25,
            "num_turns": 1,
            "usage": {
                "input_tokens": 21,
                "output_tokens": 8,
                "thinking_tokens": 3,
                "cache_read_tokens": 5,
                "total_tokens": 37,
            },
        },
    })
    .to_string();

    // Act
    let parsed = parse_antigravity_response_with_fallback(&stdout, "diagnostics");

    // Assert
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&parsed.content)
            .expect("content should remain JSON"),
        response
    );
    assert_eq!(parsed.stats.input_tokens, 21);
    assert_eq!(parsed.stats.output_tokens, 8);
    assert_eq!(parsed.stats.diff_state, SessionDiffState::Unknown);
}

#[test]
/// Ensures an empty Antigravity stream falls back to stderr diagnostics.
fn test_antigravity_parse_response_falls_back_for_empty_stdout() {
    // Arrange
    let stderr = "Antigravity emitted no result";

    // Act
    let parsed = parse_antigravity_response_with_fallback("", stderr);

    // Assert
    assert_eq!(parsed.content, stderr);
    assert_eq!(parsed.stats, SessionStats::default());
}

#[test]
/// Ensures top-level structured responses are preserved while primitive
/// values remain ineligible as response content.
fn test_antigravity_parse_response_reads_top_level_structured_response() {
    // Arrange
    let response = serde_json::json!({
        "answer": "Top-level response",
        "questions": [],
    });
    let stdout = serde_json::json!({"response": response}).to_string();

    // Act
    let parsed = parse_antigravity_response_with_fallback(&stdout, "");
    let primitive_content = antigravity_value_text(&serde_json::json!(42));

    // Assert
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&parsed.content)
            .expect("content should remain JSON"),
        response
    );
    assert_eq!(primitive_content, None);
}

#[test]
/// Ensures Antigravity JSON arrays and final `value` payloads are accepted
/// across the CLI's structured output variants.
fn test_antigravity_parse_response_reads_array_final_value() {
    // Arrange
    let response = serde_json::json!({
        "answer": "Array result",
        "questions": [],
        "review_comment_outcomes": [],
    });
    let stdout = serde_json::json!([
        {"event": "response", "delta": "partial"},
        {"event": "completed", "value": response},
    ])
    .to_string();

    // Act
    let parsed = parse_antigravity_response_with_fallback(&stdout, "");

    // Assert
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&parsed.content)
            .expect("content should remain JSON"),
        response
    );
}

#[test]
/// Ensures a direct schema-constrained Antigravity response remains
/// available without an event envelope.
fn test_antigravity_parse_response_reads_direct_protocol_payload() {
    // Arrange
    let stdout = r#"{"answer":"Direct result","questions":[]}"#;

    // Act
    let parsed = parse_antigravity_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&parsed.content)
            .expect("content should remain JSON"),
        serde_json::json!({"answer": "Direct result", "questions": []})
    );
}

#[test]
/// Ensures a direct focused-review payload supersedes preceding response
/// chunks after its schema has been validated.
fn test_antigravity_parse_response_reads_streamed_direct_focused_review() {
    // Arrange
    let review = serde_json::json!({
        "project_impact": ["Improves focused-review reliability."],
        "suggestions": [{
            "details": "Preserve the final structured review.",
            "severity": "medium",
        }],
    });
    let stdout = format!(
        "{}\n{}",
        serde_json::json!({"event": "response", "delta": "partial response"}),
        review,
    );

    // Act
    let parsed = parse_antigravity_response_with_fallback(&stdout, "");

    // Assert
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&parsed.content)
            .expect("content should remain JSON"),
        review
    );
}

#[test]
/// Ensures Antigravity response deltas are joined when no separate final
/// result envelope is emitted.
fn test_antigravity_parse_response_joins_streamed_content() {
    // Arrange
    let stdout = concat!(
        r#"{"event":"response","delta":"{\"answer\":\"Joined"}"#,
        "\n",
        r#"{"event":"response","delta":" result\",\"questions\":[]}"}"#
    );

    // Act
    let parsed = parse_antigravity_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(
        parsed.content,
        r#"{"answer":"Joined result","questions":[]}"#
    );
}

#[test]
/// Ensures Antigravity reasoning events surface as progress instead of
/// response content.
fn test_antigravity_stream_output_line_classifies_reasoning() {
    // Arrange
    let stdout_line = r#"{"event":"reasoning","value":"Inspecting files"}"#;

    // Act
    let parsed_line = parse_stream_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, Some(("Inspecting files".to_string(), false)));
}

#[test]
/// Ensures Antigravity response deltas remain classified as response
/// content so the final protocol response is rendered only once.
fn test_antigravity_stream_output_line_classifies_response_content() {
    // Arrange
    let stdout_line = r#"{"event":"response","text":"partial output"}"#;

    // Act
    let parsed_line = parse_stream_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, Some(("partial output".to_string(), true)));
}

#[test]
/// Ensures Antigravity tool events surface compact progress text.
fn test_antigravity_stream_output_line_classifies_tool_progress() {
    // Arrange
    let stdout_line = r#"{"event":"tool","tool_name":"web_search"}"#;

    // Act
    let parsed_line = parse_stream_line(stdout_line);

    // Assert
    assert_eq!(parsed_line, Some(("Searching the web".to_string(), false)));
}

#[test]
/// Ensures unstructured Antigravity diagnostics remain available when
/// structured parsing is impossible.
fn test_antigravity_parse_response_falls_back_to_raw_output() {
    // Arrange
    let stdout = "plain diagnostic";

    // Act
    let parsed = parse_antigravity_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, stdout);
    assert_eq!(parsed.stats, SessionStats::default());
}

#[test]
/// Ensures events without usable text are skipped and final labels are read
/// from every supported discriminator field.
fn test_antigravity_parse_response_reads_final_labels_and_skips_empty_events() {
    // Arrange
    let response_cases = [
        (
            concat!(
                r#"{"result":{"status":"ok"}}"#,
                "\n",
                r#"{"event":"final"}"#,
                "\n",
                r#"{"event":"progress"}"#,
                "\n",
                r#"{"state":"complete","value":"State final"}"#
            ),
            "State final",
        ),
        (r#"{"type":"result","value":"Typed final"}"#, "Typed final"),
        (r#"{"event":"progress"}"#, r#"{"event":"progress"}"#),
    ];

    for (stdout, expected_content) in response_cases {
        // Act
        let parsed = parse_antigravity_response_with_fallback(stdout, "");

        // Assert
        assert_eq!(parsed.content, expected_content, "stdout: {stdout}");
    }
}

#[test]
fn direct_utility_and_metadata_objects_survive_provider_extraction() {
    // Arrange
    let payloads = [
        serde_json::json!({"answer":"Say \"hello\"\nSecond line"}),
        serde_json::json!({"title":"Title", "description":"Quoted \"text\"\nMore", "is_title_change_significant":false}),
    ];

    for payload in payloads {
        // Act
        let parsed = parse_antigravity_response_with_fallback(&payload.to_string(), "");

        // Assert
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&parsed.content).expect("json"),
            payload
        );
    }
}

#[test]
fn protocol_payload_detection_matches_owned_deserialization() {
    // Arrange
    let payloads = [
        serde_json::json!({"answer":"Done","questions":[]}),
        serde_json::json!({"answer":"Utility"}),
        serde_json::json!({"answer":"Utility","extra":true}),
        serde_json::json!({"project_impact":[],"suggestions":[]}),
        serde_json::json!({"candidate_decisions":[{"candidate_index":0,"reason":"Duplicate","suggestion_index":null}],"project_impact":[],"suggestions":[]}),
        serde_json::json!({"candidate_decisions":[{"candidate_index":0,"reason":"Missing"}],"project_impact":[],"suggestions":[]}),
        serde_json::json!({"title":"Title","description":"Text","is_title_change_significant":true}),
        serde_json::json!({"title":"Title"}),
        serde_json::json!("plain text"),
        serde_json::json!([{"answer":"Utility"}]),
    ];

    for payload in payloads {
        // Act
        let detected = antigravity_protocol_payload(&payload);

        // Assert
        let owned_detection = (payload.get("answer").is_some()
            && payload.get("questions").is_some())
            || serde_json::from_value::<FocusedReview>(payload.clone()).is_ok()
            || serde_json::from_value::<UtilityResponse>(payload.clone()).is_ok()
            || serde_json::from_value::<ReviewMetadata>(payload.clone()).is_ok();
        assert_eq!(detected, owned_detection, "payload: {payload}");
    }
}
