use serde_json::json;

use crate::agent::app_server::gemini::usage::{
    extract_prompt_result_text, extract_prompt_usage_tokens, parse_prompt_completion_response,
};
use crate::app_server::AppServerError;

#[test]
fn prompt_result_text_flattens_nested_content_parts() {
    // Arrange
    let result = json!({
        "content": {
            "parts": [null, "First", {"content": {"text": " second"}}]
        }
    });

    // Act
    let text = extract_prompt_result_text(&result);

    // Assert
    assert_eq!(text.as_deref(), Some("First second"));
}

#[test]
fn prompt_result_text_reads_message_content() {
    // Arrange
    let result = json!({"message": {"content": ["Message text"]}});

    // Act
    let text = extract_prompt_result_text(&result);

    // Assert
    assert_eq!(text.as_deref(), Some("Message text"));
}

#[test]
fn prompt_result_text_combines_output_items() {
    // Arrange
    let result = json!({
        "output": [
            {"text": "First"},
            {"content": {"text": " second"}}
        ]
    });

    // Act
    let text = extract_prompt_result_text(&result);

    // Assert
    assert_eq!(text.as_deref(), Some("First second"));
}

#[test]
fn prompt_completion_requires_a_result_payload() {
    // Arrange
    let response = json!({"jsonrpc": "2.0", "id": "prompt-1"});

    // Act
    let result = parse_prompt_completion_response(&response);

    // Assert
    assert!(matches!(
        result,
        Err(AppServerError::Provider(message))
            if message == "Gemini ACP `session/prompt` response missing `result`"
    ));
}

#[test]
fn prompt_usage_tokens_read_every_supported_usage_shape() {
    for (result, expected_tokens) in [
        (
            json!({"usage": {"input_tokens": 3, "output_tokens": 4}}),
            (3, 4),
        ),
        (
            json!({"_meta": {"quota": {"tokenCount": {"inputTokens": 5, "outputTokens": 6}}}}),
            (5, 6),
        ),
        (
            json!({"_meta": {"quota": {"model_usage": [
                {"token_count": {"inputTokens": 1, "outputTokens": 2}},
                {"tokenCount": {"input_tokens": 3, "output_tokens": 4}},
                {"model": "no-usage"}
            ]}}}),
            (4, 6),
        ),
        (
            json!({"_meta": {"quota": {"modelUsage": [{"model": "no-usage"}]}}}),
            (0, 0),
        ),
        (
            json!({"_meta": {"quota": {"model_usage": "unavailable"}}}),
            (0, 0),
        ),
        (json!({}), (0, 0)),
    ] {
        // Act
        let tokens = extract_prompt_usage_tokens(&result);

        // Assert
        assert_eq!(tokens, expected_tokens, "unexpected usage for {result}");
    }
}

#[test]
fn prompt_result_text_ignores_output_items_without_text() {
    // Arrange
    let result = json!({"output": [{"type": "reasoning"}, {"content": 7}]});

    // Act
    let text = extract_prompt_result_text(&result);

    // Assert
    assert_eq!(text, None);
}
