//! Anthropic Messages response decoding, usage mapping, and thinking replay
//! tests.

use serde_json::{Value, json};

use super::{AnthropicBackend, MessagesResponse, default_transport, reasoning_view, replayed_turn};
use crate::model::{GeneratedResponse, ModelError};
use crate::provider_spec::{ProviderSpec, WireProtocol};
use crate::tool::ToolCall;

/// Decodes `response` through a backend with the default policy, reading a
/// `structured_output` call as the answer when `output_tool` holds.
fn decode(response: &Value, output_tool: bool) -> GeneratedResponse {
    let spec = ProviderSpec::new(
        "custom",
        WireProtocol::AnthropicMessages,
        "https://example.test/v1",
    );
    let backend = AnthropicBackend::new(
        &spec,
        "claude".to_string(),
        reqwest::header::HeaderMap::new(),
        default_transport(),
    );
    let response = serde_json::from_value::<MessagesResponse>(response.clone())
        .expect("the response should decode");

    backend.decode(&response, output_tool)
}

/// Returns the error of a failed response.
fn failure(response: GeneratedResponse) -> ModelError {
    match response {
        GeneratedResponse::Failed { error, .. } => Some(error),
        GeneratedResponse::Output { .. }
        | GeneratedResponse::ToolCall { .. }
        | GeneratedResponse::ToolCalls { .. } => None,
    }
    .expect("the response should fail")
}

/// Returns a call to `read` with `arguments` under `id`.
fn call(id: &str, arguments: &str) -> ToolCall {
    ToolCall::from_json(id.to_string(), "read", arguments, None).expect("the call should decode")
}

#[test]
fn reads_only_thinking_records_as_anthropic_reasoning() {
    // Arrange
    let markers_only = json!([{"type": "text"}, {"type": "tool_use", "id": "toolu_1"}]).to_string();
    let foreign = json!([{"type": "thinking", "thinking": "a"}, {"type": "reasoning"}]).to_string();

    // Act
    let views = [
        reasoning_view(&markers_only),
        reasoning_view(&foreign),
        reasoning_view("plain text"),
        reasoning_view(r#"[{"thinking": "untyped"}]"#),
    ];

    // Assert
    assert_eq!(views, [None, None, None, None]);
}

#[test]
fn replays_text_before_calls_without_a_thinking_record() {
    // Arrange
    let calls = [call("toolu_1", r#"{"path":"a.txt"}"#)];

    // Act
    let blocks = replayed_turn(Some("cleared"), Some("Reading."), &calls);

    // Assert
    assert_eq!(
        blocks,
        [
            json!({"type": "text", "text": "Reading."}),
            json!({"type": "tool_use", "id": "toolu_1", "name": "read", "input": {"path": "a.txt"}}),
        ]
    );
}

#[test]
fn replays_unplaced_text_and_calls_after_the_recorded_blocks() {
    // Arrange
    let record = json!([
        {"type": "thinking", "thinking": "", "signature": "sig"},
        {"type": "tool_use", "id": "toolu_gone"},
        {"type": "tool_use", "id": "toolu_1"},
        {"type": "tool_use", "id": "toolu_1"}
    ])
    .to_string();
    let calls = [call("toolu_1", "{}"), call("toolu_2", "not json")];

    // Act
    let blocks = replayed_turn(Some(&record), Some("Late text."), &calls);

    // Assert
    assert_eq!(
        blocks,
        [
            json!({"type": "thinking", "thinking": "", "signature": "sig"}),
            json!({"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}),
            json!({"type": "text", "text": "Late text."}),
            json!({
                "type": "tool_use",
                "id": "toolu_2",
                "name": "read",
                "input": {"malformed_arguments": "not json"}
            }),
        ]
    );
}

#[test]
fn leaves_out_blank_recorded_text() {
    // Arrange
    let record = json!([
        {"type": "redacted_thinking", "data": "opaque"},
        {"type": "text"}
    ])
    .to_string();

    // Act
    let blocks = replayed_turn(Some(&record), Some("  "), &[]);

    // Assert
    assert_eq!(
        blocks,
        [json!({"type": "redacted_thinking", "data": "opaque"})]
    );
}

#[test]
fn rejects_unsupported_calls_blank_answers_and_calls_beside_a_forced_answer() {
    // Arrange
    let server_call = json!({
        "content": [{"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search"}],
        "stop_reason": "end_turn"
    });
    let blank = json!({"content": [{"type": "text", "text": " "}], "stop_reason": "end_turn"});
    let mixed = json!({
        "content": [
            {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}},
            {"type": "tool_use", "id": "toolu_2", "name": "structured_output", "input": {}}
        ],
        "stop_reason": "tool_use"
    });

    // Act
    let errors = [
        failure(decode(&server_call, false)),
        failure(decode(&blank, false)),
        failure(decode(&mixed, true)),
    ];

    // Assert
    assert!(matches!(
        &errors[0],
        ModelError::UnsupportedToolType { kind } if kind == "server_tool_use"
    ));
    assert!(matches!(errors[1], ModelError::InvalidResponse));
    assert!(matches!(
        errors[2],
        ModelError::TerminalResponseWithToolCalls
    ));
}

#[test]
fn rejects_malformed_and_duplicate_calls() {
    // Arrange
    let malformed = json!({
        "content": [{"type": "tool_use", "name": "read", "input": {}}],
        "stop_reason": "tool_use"
    });
    let duplicate = json!({
        "content": [
            {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}},
            {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}
        ],
        "stop_reason": "tool_use"
    });

    // Act
    let malformed = failure(decode(&malformed, false));
    let duplicate = failure(decode(&duplicate, false));

    // Assert
    assert!(matches!(malformed, ModelError::InvalidToolArguments { .. }));
    assert!(matches!(duplicate, ModelError::DuplicateToolCallId { .. }));
}

#[test]
fn reports_a_paused_turn_and_an_unexplained_refusal_as_incomplete() {
    // Arrange
    let paused = json!({"content": [], "stop_reason": "pause_turn"});
    let refused = json!({"content": null, "stop_reason": "refusal", "stop_details": null});

    // Act
    let paused = failure(decode(&paused, false));
    let refused = failure(decode(&refused, false));

    // Assert
    assert!(matches!(
        paused,
        ModelError::IncompleteResponse { reason } if reason == "pause_turn"
    ));
    assert!(matches!(
        refused,
        ModelError::IncompleteResponse { reason } if reason == "refusal"
    ));
}

#[test]
fn ignores_unknown_blocks_and_leaves_input_and_total_unknown_without_uncached_input() {
    // Arrange
    let response = json!({
        "content": [{"type": "fallback"}, {"type": "text", "text": "Ada"}],
        "stop_reason": "end_turn",
        "usage": {"cache_read_input_tokens": 7, "output_tokens": 3}
    });

    // Act
    let metadata = match decode(&response, false) {
        GeneratedResponse::Output { metadata, .. } => Some(metadata),
        GeneratedResponse::Failed { .. }
        | GeneratedResponse::ToolCall { .. }
        | GeneratedResponse::ToolCalls { .. } => None,
    };

    // Assert
    let usage = metadata
        .and_then(|metadata| metadata.usage().copied())
        .expect("usage should be kept");
    assert_eq!(usage.cache_hit_tokens(), Some(7));
    assert_eq!(usage.cache_write_tokens(), None);
    assert_eq!(usage.input_tokens(), None);
    assert_eq!(usage.total_tokens(), None);
}
