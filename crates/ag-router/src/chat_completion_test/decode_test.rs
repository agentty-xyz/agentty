//! Chat Completions response decoding tests.

use crate::chat_completion::ChatCompletionBackend;
use crate::model::ModelError;
use crate::schema::RESPONSE_CONTENT_LIMIT_BYTES;

#[test]
fn oversized_content_is_reported_before_a_missing_tool_call() {
    // Arrange
    let oversized = "x".repeat(RESPONSE_CONTENT_LIMIT_BYTES + 1);

    // Act
    let oversized_error =
        ChatCompletionBackend::decode_tool_call_parts(Some(&oversized), None, vec![]);
    let missing_error = ChatCompletionBackend::decode_tool_call_parts(Some("notes"), None, vec![]);

    // Assert
    assert!(matches!(
        oversized_error,
        Err(ModelError::ResponseContentTooLarge)
    ));
    assert!(matches!(missing_error, Err(ModelError::MissingToolCall)));
}
