use std::io;

use serde_json::json;

use crate::model::{
    ErrorClass, JsonSchemaFormat, ModelError, ModelMessage, ModelRequest, ReasoningEffort,
    ensure_unique_tool_call_ids,
};
use crate::schema::OutputSchema;
use crate::tool::{ToolCall, ToolDefinition};

fn format() -> JsonSchemaFormat {
    JsonSchemaFormat::new(
        "answer",
        OutputSchema::new(json!({"type":"object"})).expect("valid schema"),
    )
    .expect("valid name")
}

#[test]
fn requires_bounded_response_format_name() {
    // Arrange
    let schema = OutputSchema::new(json!({"type":"object"})).expect("valid schema");

    // Act / Assert
    assert!(matches!(
        JsonSchemaFormat::new("  ", schema.clone()),
        Err(ModelError::InvalidResponseFormatName)
    ));
    assert!(matches!(
        JsonSchemaFormat::new("x".repeat(129), schema),
        Err(ModelError::InvalidResponseFormatName)
    ));
    assert_eq!(format().name(), "answer");
}

#[test]
fn chat_request_exposes_required_schema_messages_and_tools() {
    // Arrange
    let tool = ToolDefinition::new("lookup", "Lookup", json!({"type":"object"}));

    // Act
    let mut request = ModelRequest::chat(
        "muse/muse-spark-1.3",
        vec![ModelMessage::User("hello".to_string())],
        vec![tool],
        format(),
    );
    request.options.reasoning_effort = Some(ReasoningEffort::High);

    // Assert
    assert_eq!(request.messages().len(), 1);
    assert_eq!(request.tools()[0].name(), "lookup");
    assert!(request.schema().has_object_root());
    assert_eq!(
        request.model_reasoning_effort(),
        Some(ReasoningEffort::High)
    );
    assert!(request.advertises_tool("lookup"));
    assert!(!request.advertises_tool("other"));
}

#[test]
fn rejects_duplicate_function_call_ids() {
    // Arrange
    let call =
        ToolCall::from_json("same".to_string(), "lookup", "{}", None).expect("valid function call");

    // Act
    let result = ensure_unique_tool_call_ids(&[call.clone(), call]);

    // Assert
    assert!(matches!(result, Err(ModelError::DuplicateToolCallId { id }) if id == "same"));
}

#[test]
fn classifies_provider_independent_errors() {
    // Arrange
    let cases = [
        (ModelError::InvalidModelId, ErrorClass::Request),
        (ModelError::InvalidResponse, ErrorClass::InvalidResponse),
        (
            ModelError::ResponseBodyTooLarge,
            ErrorClass::ResponseTooLarge,
        ),
        (
            ModelError::UnsupportedImageInput {
                reason: "unsupported".to_string(),
            },
            ErrorClass::UnsupportedInput,
        ),
        (
            ModelError::UnsupportedOutputSchema {
                reason: "unsupported".to_string(),
            },
            ErrorClass::UnsupportedOutput,
        ),
        (
            ModelError::InvalidJson {
                reason: "invalid".to_string(),
            },
            ErrorClass::InvalidOutput,
        ),
        (ModelError::InvalidToolCallId, ErrorClass::InvalidToolCall),
    ];

    // Act / Assert
    for (error, expected) in cases {
        assert_eq!(error.error_class(), expected);
        assert_eq!(error.http_status(), None);
    }
}

#[test]
fn reasoning_efforts_have_stable_wire_names() {
    // Arrange
    let cases = [
        (ReasoningEffort::Low, "low"),
        (ReasoningEffort::Medium, "medium"),
        (ReasoningEffort::High, "high"),
        (ReasoningEffort::XHigh, "xhigh"),
        (ReasoningEffort::Max, "max"),
    ];

    // Act / Assert
    for (effort, name) in cases {
        assert_eq!(effort.as_str(), name);
    }
}

#[test]
fn preserves_request_sources_with_and_without_response_metadata() {
    // Arrange
    let request = ModelError::request(io::Error::other("offline"));
    let wrapped = ModelError::InvalidResponse.with_metadata(crate::model::CompletionMetadata::new(
        "stop".to_string(),
        None,
        None,
        None,
        None,
    ));

    // Act
    let request_source = request.into_source();
    let wrapped_source = wrapped.into_source();
    let plain_source = ModelError::InvalidResponse.into_source();

    // Assert
    assert_eq!(request_source.to_string(), "offline");
    assert_eq!(
        wrapped_source.to_string(),
        "model returned no response content"
    );
    assert_eq!(
        plain_source.to_string(),
        "model returned no response content"
    );
}
