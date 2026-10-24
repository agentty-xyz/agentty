use serde_json::json;

use crate::chat_completion::ChatCompletionBackend;
use crate::model::{JsonSchemaFormat, ModelError, ModelMessage, ModelRequest};
use crate::schema::{OutputSchema, RESPONSE_CONTENT_LIMIT_BYTES};

#[test]
fn oversized_content_is_reported_before_a_missing_tool_call() {
    // Arrange
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    let request = ModelRequest::chat(
        "qwen/qwen-plus",
        vec![ModelMessage::User("inspect".to_string())],
        vec![],
        JsonSchemaFormat::new("output", schema).expect("format"),
    );
    let oversized = "x".repeat(RESPONSE_CONTENT_LIMIT_BYTES + 1);

    // Act
    let oversized_error =
        ChatCompletionBackend::decode_tool_call_parts(&request, Some(&oversized), None, vec![]);
    let missing_error =
        ChatCompletionBackend::decode_tool_call_parts(&request, Some("notes"), None, vec![]);

    // Assert
    assert!(matches!(
        oversized_error,
        Err(ModelError::ResponseContentTooLarge)
    ));
    assert!(matches!(missing_error, Err(ModelError::MissingToolCall)));
}
