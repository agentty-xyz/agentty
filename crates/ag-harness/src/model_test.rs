use std::io;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    CompletionMetadata, CompletionUsage, Model, ModelClient, ModelCompletion, ModelError,
    ModelErrorType, ModelMessage, ModelMetadata, ModelMetadataError, ModelRequest, ModelResponse,
    ReasoningEffort,
};
use crate::lifecycle::LifecycleEventKind;
use crate::provider::QwenConfig;
use crate::schema_contract::{OutputSchema, OutputValidationError};
use crate::tool;
use crate::tool::{ReadArguments, ToolCall};

struct ResponseOnlyModel;

#[async_trait]
impl Model for ResponseOnlyModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({ "name": "Ada" }),
        )))
    }
}

struct MetadataModel;

#[async_trait]
impl Model for MetadataModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        Ok(ModelCompletion::new(
            CompletionMetadata::new(
                "stop".to_string(),
                Some("response-id".to_string()),
                None,
                None,
                None,
            ),
            ModelResponse::Output(json!({ "name": "Ada" })),
        ))
    }
}

fn test_request() -> ModelRequest {
    let schema =
        OutputSchema::new(json!({ "type": "object" })).expect("fixture schema should be valid");

    ModelRequest::new("prompt", schema)
}

#[tokio::test]
async fn response_only_model_returns_completion_without_metadata() {
    // Arrange
    let model = ResponseOnlyModel;

    // Act
    let model_metadata = Model::metadata(&model);
    let completion = model
        .complete(test_request())
        .await
        .expect("response-only model should complete");

    // Assert
    assert!(model_metadata.is_none());
    assert_eq!(
        completion.response().output(),
        Some(&json!({ "name": "Ada" }))
    );
    assert!(completion.metadata().is_none());
}

#[tokio::test]
async fn model_completion_exposes_optional_metadata() {
    // Arrange
    let model = MetadataModel;

    // Act
    let model_metadata = Model::metadata(&model);
    let completion = Model::complete(&model, test_request())
        .await
        .expect("metadata model should complete through Model");

    // Assert
    assert!(model_metadata.is_none());
    assert_eq!(
        completion.response().output(),
        Some(&json!({ "name": "Ada" }))
    );
    assert_eq!(
        completion
            .metadata()
            .and_then(CompletionMetadata::response_id),
        Some("response-id")
    );
}

#[test]
fn client_exposes_provider_and_model() {
    // Arrange
    let client = ModelClient::qwen(QwenConfig {
        api_key: "test-key".to_string(),
        base_url: "https://example.com".to_string(),
        model: "qwen-plus".to_string(),
    })
    .expect("fixture configuration should be valid");

    // Act
    let metadata = client.metadata();
    let trait_metadata = Model::metadata(&client)
        .expect("model client should expose configured metadata through the trait");

    // Assert
    assert_eq!(metadata.provider(), "alibaba_cloud");
    assert_eq!(metadata.model(), "qwen-plus");
    assert_eq!(
        metadata,
        &ModelMetadata::new("alibaba_cloud", "qwen-plus").expect("metadata should be valid")
    );
    assert_eq!(&trait_metadata, metadata);
}

#[tokio::test]
async fn client_observes_success_unless_request_is_already_observed() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Ada"}"#}
            }]
        })))
        .expect(2)
        .mount(&server)
        .await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let client = ModelClient::qwen(QwenConfig {
        api_key: "test-key".to_string(),
        base_url: server.uri(),
        model: "qwen-plus".to_string(),
    })
    .expect("fixture configuration should be valid")
    .with_lifecycle_observer(move |event| {
        observed_events
            .lock()
            .expect("event recorder should not be poisoned")
            .push(event);
    });
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"]
    }))
    .expect("fixture schema should be valid");
    let mut observed_request = ModelRequest::new("prompt", schema.clone());
    observed_request.mark_lifecycle_observed();

    // Act
    client
        .complete(ModelRequest::new("prompt", schema))
        .await
        .expect("request should succeed");
    client
        .complete(observed_request)
        .await
        .expect("externally observed request should succeed");

    // Assert
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].kind(),
        LifecycleEventKind::ModelRequestStarted { .. }
    ));
    assert!(matches!(
        events[1].kind(),
        LifecycleEventKind::ModelRequestCompleted { .. }
    ));
}

#[tokio::test]
async fn client_returns_multiple_tool_calls() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_manifest",
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": r#"{"path":"Cargo.toml"}"#
                            }
                        },
                        {
                            "id": "call_readme",
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": r#"{"path":"README.md"}"#
                            }
                        }
                    ]
                }
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = ModelClient::qwen(QwenConfig {
        api_key: "test-key".to_string(),
        base_url: server.uri(),
        model: "qwen-plus".to_string(),
    })
    .expect("fixture configuration should be valid");
    let request = test_request().with_tool(tool::ToolDefinition::read());

    // Act
    let completion = client
        .complete(request)
        .await
        .expect("batched tool response should complete");

    // Assert
    assert_eq!(
        completion
            .metadata()
            .expect("provider completion should include metadata")
            .finish_reason(),
        "tool_calls"
    );
    assert_eq!(
        completion
            .response()
            .calls()
            .iter()
            .map(tool::ToolCall::id)
            .collect::<Vec<_>>(),
        ["call_manifest", "call_readme"]
    );
}

#[tokio::test]
async fn client_observes_classified_failure() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("offline"))
        .expect(1)
        .mount(&server)
        .await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let client = ModelClient::qwen(QwenConfig {
        api_key: "test-key".to_string(),
        base_url: server.uri(),
        model: "qwen-plus".to_string(),
    })
    .expect("fixture configuration should be valid")
    .with_lifecycle_observer(move |event| {
        observed_events
            .lock()
            .expect("event recorder should not be poisoned")
            .push(event);
    });
    let schema =
        OutputSchema::new(json!({ "type": "object" })).expect("fixture schema should be valid");

    // Act
    let error = client
        .complete(ModelRequest::new("prompt", schema))
        .await
        .expect_err("provider failure should be returned");

    // Assert
    assert_eq!(error.error_type(), ModelErrorType::Provider);
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[1].kind(),
        LifecycleEventKind::ModelRequestFailed {
            error_type: ModelErrorType::Provider,
            http_status: Some(503),
            ..
        }
    ));
}

#[tokio::test]
async fn client_supports_dynamic_model_dispatch() {
    // Arrange
    let model: Box<dyn Model> = Box::new(
        ModelClient::qwen(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: "https://example.com".to_string(),
            model: "qwen-plus".to_string(),
        })
        .expect("fixture configuration should be valid"),
    );
    let schema = OutputSchema::new(json!({ "type": "array" })).expect("schema should be valid");

    // Act
    let error = model
        .complete(ModelRequest::new("return a list", schema))
        .await
        .expect_err("Qwen should reject a non-object schema");

    // Assert
    assert!(matches!(error, ModelError::UnsupportedOutputSchema { .. }));
}

#[test]
fn metadata_rejects_empty_provider() {
    // Arrange and Act
    let error =
        ModelMetadata::new("  ", "stub-large").expect_err("empty provider should be rejected");

    // Assert
    assert_eq!(error, ModelMetadataError::EmptyProvider);
    assert_eq!(error.to_string(), "model provider must not be empty");
}

#[test]
fn metadata_rejects_empty_model() {
    // Arrange and Act
    let error =
        ModelMetadata::new("stub_provider", "  ").expect_err("empty model should be rejected");

    // Assert
    assert_eq!(error, ModelMetadataError::EmptyModel);
    assert_eq!(error.to_string(), "model identifier must not be empty");
}

#[test]
fn request_contains_prompt_and_schema() {
    // Arrange
    let schema = OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

    // Act
    let request = ModelRequest::new("hello", schema.clone());

    // Assert
    assert_eq!(request.prompt(), "hello");
    assert_eq!(request.schema(), &schema);
    assert_eq!(request.tools(), []);
}

#[test]
fn request_uses_provider_neutral_reasoning_effort_names() {
    // Arrange
    let schema = OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");
    let names = ["low", "medium", "high", "xhigh", "max"];

    // Act
    let request =
        ModelRequest::new("hello", schema).with_model_reasoning_effort(ReasoningEffort::High);
    let serialized = ReasoningEffort::ALL
        .iter()
        .map(|effort| serde_json::to_value(effort).expect("effort should serialize"))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        request.model_reasoning_effort(),
        Some(ReasoningEffort::High)
    );
    assert_eq!(
        serialized,
        names.map(|name| serde_json::Value::String(name.to_string()))
    );
    assert_eq!(ReasoningEffort::default(), ReasoningEffort::High);
    assert_eq!(ReasoningEffort::ALL.map(ReasoningEffort::as_str), names);
}

#[test]
fn request_explicitly_advertises_read() {
    // Arrange
    let schema = OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

    // Act
    let request = ModelRequest::new("hello", schema).with_tool(tool::ToolDefinition::read());

    // Assert
    assert_eq!(request.tools(), &[tool::ToolDefinition::read()]);
    assert!(request.advertises_tool("read"));
    assert!(!request.advertises_tool("write"));
}

#[test]
fn request_deduplicates_native_tools() {
    // Arrange
    let schema = OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

    // Act
    let request = ModelRequest::new("hello", schema)
        .with_tool(tool::ToolDefinition::read())
        .with_tool(tool::ToolDefinition::read());

    // Assert
    assert_eq!(request.tools(), &[tool::ToolDefinition::read()]);
}

#[test]
fn response_exposes_terminal_output() {
    // Arrange
    let value = json!({ "name": "Ada" });

    // Act
    let response = ModelResponse::from_output(value.clone());

    // Assert
    assert_eq!(response.output(), Some(&value));
    assert!(response.call().is_none());
    assert_eq!(response.calls(), []);
}

#[test]
fn response_exposes_multiple_tool_calls() {
    // Arrange
    let calls = vec![
        tool::ToolCall::read(
            "call_one".to_string(),
            serde_json::from_value(json!({"path": "Cargo.toml"}))
                .expect("read arguments should be valid"),
            None,
        ),
        tool::ToolCall::read(
            "call_two".to_string(),
            serde_json::from_value(json!({"path": "README.md"}))
                .expect("read arguments should be valid"),
            None,
        ),
    ];

    // Act
    let response = ModelResponse::tool_calls(calls);

    // Assert
    assert!(response.output().is_none());
    assert!(response.call().is_none());
    assert_eq!(
        response
            .calls()
            .iter()
            .map(tool::ToolCall::id)
            .collect::<Vec<_>>(),
        ["call_one", "call_two"]
    );
}

#[test]
fn batched_tool_message_retained_bytes_sums_each_call() {
    // Arrange
    let calls = vec![
        tool::ToolCall::read(
            "call_manifest".to_string(),
            serde_json::from_value(json!({"path": "Cargo.toml"}))
                .expect("read arguments should be valid"),
            Some("reasoning".to_string()),
        ),
        tool::ToolCall::read(
            "call_readme".to_string(),
            serde_json::from_value(json!({"path": "README.md"}))
                .expect("read arguments should be valid"),
            None,
        ),
    ];
    let message = ModelMessage::AssistantToolCalls(calls);
    let expected = "call_manifest".len()
        + "read".len()
        + r#"{"path":"Cargo.toml"}"#.len()
        + "reasoning".len()
        + "call_readme".len()
        + "read".len()
        + r#"{"path":"README.md"}"#.len();

    // Act
    let retained_bytes = message.retained_bytes();

    // Assert
    assert_eq!(retained_bytes, expected);
}

#[test]
fn completion_exposes_normalized_metadata_and_response() {
    // Arrange
    let usage = CompletionUsage::new(Some(5), Some(8), Some(13), Some(21), Some(3), Some(34));
    let metadata = CompletionMetadata::new(
        "stop".to_string(),
        Some("response-1".to_string()),
        Some("provider-model".to_string()),
        Some("fingerprint-1".to_string()),
        Some(usage),
    );
    let response = ModelResponse::from_output(json!({ "name": "Ada" }));
    let completion = ModelCompletion::new(metadata, response.clone())
        .with_provider_session_id("provider-session-1");

    // Act
    let completion_metadata = completion
        .metadata()
        .expect("completion should include metadata");
    let completion_response = completion.response();

    // Assert
    assert_eq!(completion_metadata.finish_reason(), "stop");
    assert_eq!(completion_metadata.response_id(), Some("response-1"));
    assert_eq!(completion_metadata.response_model(), Some("provider-model"));
    assert_eq!(
        completion_metadata.system_fingerprint(),
        Some("fingerprint-1")
    );
    assert_eq!(completion_metadata.usage(), Some(&usage));
    assert_eq!(completion.provider_session_id(), Some("provider-session-1"));
    assert_eq!(usage.cache_hit_tokens(), Some(5));
    assert_eq!(usage.cache_miss_tokens(), Some(8));
    assert_eq!(usage.input_tokens(), Some(13));
    assert_eq!(usage.output_tokens(), Some(21));
    assert_eq!(usage.reasoning_tokens(), Some(3));
    assert_eq!(usage.total_tokens(), Some(34));
    assert_eq!(completion_response, &response);
    assert_eq!(completion.into_response(), response);
}

#[test]
fn completion_metadata_preserves_absent_provider_fields() {
    // Arrange
    let metadata = CompletionMetadata::new("stop".to_string(), None, None, None, None);

    // Act and Assert
    assert_eq!(metadata.response_id(), None);
    assert_eq!(metadata.response_model(), None);
    assert_eq!(metadata.system_fingerprint(), None);
    assert_eq!(metadata.usage(), None);
}

#[test]
fn response_debug_redacts_provider_reasoning() {
    // Arrange
    let secret_reasoning = "private reasoning from repository context";
    let arguments = serde_json::from_value::<ReadArguments>(json!({
        "path": "Cargo.toml"
    }))
    .expect("read arguments should be valid");
    let response = ModelResponse::tool_call(ToolCall::read(
        "call_read".to_string(),
        arguments,
        Some(secret_reasoning.to_string()),
    ));

    // Act
    let debug_output = format!("{response:?}");

    // Assert
    assert_eq!(response.calls()[0].id(), "call_read");
    assert!(debug_output.contains("call_read"));
    assert!(debug_output.contains("[REDACTED]"));
    assert!(!debug_output.contains(secret_reasoning));
}

#[test]
fn invalid_response_error_has_user_facing_message() {
    // Arrange and Act
    let message = ModelError::InvalidResponse.to_string();

    // Assert
    assert_eq!(message, "model returned no response content");
}

#[test]
fn incomplete_response_error_includes_reason() {
    // Arrange and Act
    let message = ModelError::IncompleteResponse {
        reason: "length".to_string(),
    }
    .to_string();

    // Assert
    assert_eq!(message, "model response is incomplete: length");
}

#[test]
fn request_error_includes_source_message() {
    // Arrange
    let source = io::Error::other("connection refused");

    // Act
    let message = ModelError::request(source).to_string();

    // Assert
    assert_eq!(message, "model request failed: connection refused");
}

#[test]
fn classifies_model_errors_with_stable_telemetry_values() {
    // Arrange
    let errors = [
        (
            ModelError::request(io::Error::other("request")),
            ModelErrorType::Request,
        ),
        (ModelError::InvalidResponse, ModelErrorType::InvalidResponse),
        (
            ModelError::IncompleteResponse {
                reason: "length".to_string(),
            },
            ModelErrorType::InvalidResponse,
        ),
        (ModelError::ResumeUnavailable, ModelErrorType::Provider),
        (
            ModelError::ResponseBodyTooLarge,
            ModelErrorType::ResponseTooLarge,
        ),
        (
            ModelError::ResponseContentTooLarge,
            ModelErrorType::ResponseTooLarge,
        ),
        (
            ModelError::UnsupportedOutputSchema {
                reason: "object required".to_string(),
            },
            ModelErrorType::UnsupportedOutput,
        ),
        (
            ModelError::InvalidJson {
                reason: "invalid".to_string(),
            },
            ModelErrorType::InvalidOutput,
        ),
        (
            ModelError::SchemaViolation {
                path: "$".to_string(),
                reason: "invalid".to_string(),
            },
            ModelErrorType::InvalidOutput,
        ),
        (ModelError::MissingToolCall, ModelErrorType::InvalidToolCall),
        (
            ModelError::MultipleToolCalls,
            ModelErrorType::InvalidToolCall,
        ),
        (
            ModelError::TerminalResponseWithToolCalls,
            ModelErrorType::InvalidToolCall,
        ),
        (
            ModelError::UnsupportedToolType {
                kind: "custom".to_string(),
            },
            ModelErrorType::InvalidToolCall,
        ),
        (
            ModelError::UnsupportedToolName {
                name: "write".to_string(),
            },
            ModelErrorType::InvalidToolCall,
        ),
        (
            ModelError::InvalidToolArguments {
                reason: "invalid".to_string(),
            },
            ModelErrorType::InvalidToolCall,
        ),
    ];

    // Act
    let classifications =
        errors.map(|(error, expected)| (error.error_type(), expected, error.http_status()));

    // Assert
    assert!(
        classifications
            .into_iter()
            .all(|(actual, expected, status)| actual == expected && status.is_none())
    );
    assert_eq!(ModelErrorType::Request.as_str(), "request_error");
    assert_eq!(ModelErrorType::Transport.as_str(), "transport_error");
    assert_eq!(ModelErrorType::Provider.as_str(), "provider_error");
    assert_eq!(
        ModelErrorType::InvalidProviderResponse.as_str(),
        "invalid_provider_response"
    );
    assert_eq!(ModelErrorType::InvalidResponse.as_str(), "invalid_response");
    assert_eq!(
        ModelErrorType::UnsupportedOutput.as_str(),
        "unsupported_output"
    );
    assert_eq!(
        ModelErrorType::ResponseTooLarge.as_str(),
        "response_too_large"
    );
    assert_eq!(ModelErrorType::InvalidOutput.as_str(), "invalid_output");
    assert_eq!(
        ModelErrorType::InvalidToolCall.as_str(),
        "invalid_tool_call"
    );
}

#[test]
fn classifies_duplicate_tool_call_id_as_invalid_tool_call() {
    // Arrange
    let error = ModelError::DuplicateToolCallId {
        id: "duplicate_call".to_string(),
    };

    // Act
    let error_type = error.error_type();

    // Assert
    assert_eq!(error_type, ModelErrorType::InvalidToolCall);
    assert_eq!(error.http_status(), None);
}

#[test]
fn classified_request_retains_source_type_and_status() {
    // Arrange
    let error = ModelError::classified_request(
        ModelErrorType::Transport,
        Some(503),
        io::Error::other("connection reset").into(),
    );

    // Act
    let source = std::error::Error::source(&error)
        .and_then(std::error::Error::source)
        .expect("classified request should retain its original source");

    // Assert
    assert_eq!(error.error_type(), ModelErrorType::Transport);
    assert_eq!(error.http_status(), Some(503));
    assert_eq!(source.to_string(), "connection reset");
}

#[test]
fn unsupported_schema_error_includes_reason() {
    // Arrange and Act
    let message = ModelError::UnsupportedOutputSchema {
        reason: "top-level object required".to_string(),
    }
    .to_string();

    // Assert
    assert_eq!(
        message,
        "provider cannot satisfy this output schema: top-level object required"
    );
}

#[test]
fn oversized_response_body_error_has_user_facing_message() {
    // Arrange and Act
    let message = ModelError::ResponseBodyTooLarge.to_string();

    // Assert
    assert_eq!(message, "model response body exceeds the size limit");
}

#[test]
fn converts_invalid_json_error() {
    // Arrange
    let error = OutputValidationError::InvalidJson("expected value".to_string());

    // Act
    let error = ModelError::from(error);

    // Assert
    assert_eq!(
        error.to_string(),
        "model returned invalid JSON: expected value"
    );
}

#[test]
fn converts_schema_violation_error() {
    // Arrange
    let error = OutputValidationError::SchemaViolation {
        path: "/name".to_string(),
        reason: "wrong type".to_string(),
    };

    // Act
    let error = ModelError::from(error);

    // Assert
    assert_eq!(
        error.to_string(),
        "model output violates the schema at /name: wrong type"
    );
}

#[test]
fn converts_oversized_content_error() {
    // Arrange and Act
    let error = ModelError::from(OutputValidationError::TooLarge);

    // Assert
    assert_eq!(
        error.to_string(),
        "model response content exceeds the size limit"
    );
}
