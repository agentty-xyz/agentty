use std::sync::Arc;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{StubClient, escaped_value_schema, qwen, request};
use crate::chat_completion::{
    ChatCompletionBackend, GeneratedResponse, RESPONSE_ENVELOPE_LIMIT_BYTES,
};
use crate::provider::qwen::{QwenConfig, policy};
use crate::schema_contract::OutputSchema;
use crate::{model, schema_contract};

#[test]
fn rejects_empty_model_during_construction() {
    // Arrange
    let config = QwenConfig {
        api_key: "test-key".to_string(),
        base_url: "https://example.com".to_string(),
        model: "  ".to_string(),
    };

    // Act
    let error = model::ModelClient::qwen(config)
        .err()
        .expect("empty model configuration should be rejected");

    // Assert
    assert_eq!(error, model::ModelMetadataError::EmptyModel);
}

#[tokio::test]
async fn completes_with_normalized_provider_metadata() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Ada"}"#}
            }],
            "id": "response-1",
            "model": "qwen-plus-2026-08-16",
            "system_fingerprint": "fingerprint-1",
            "usage": {
                "completion_tokens": 9,
                "completion_tokens_details": {"reasoning_tokens": 2},
                "prompt_tokens": 12,
                "prompt_tokens_details": {"cached_tokens": 4},
                "total_tokens": 21
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let model: Box<dyn model::Model> = Box::new(qwen(&server));

    // Act
    let completion = model
        .complete(request("extract the name"))
        .await
        .expect("Qwen completion metadata should decode");
    let metadata = completion
        .metadata()
        .expect("Qwen completion should include metadata");
    let usage = metadata.usage().expect("Qwen usage should be retained");

    // Assert
    assert_eq!(
        completion.response().output(),
        Some(&json!({ "name": "Ada" }))
    );
    assert_eq!(metadata.finish_reason(), "stop");
    assert_eq!(metadata.response_id(), Some("response-1"));
    assert_eq!(metadata.response_model(), Some("qwen-plus-2026-08-16"));
    assert_eq!(metadata.system_fingerprint(), Some("fingerprint-1"));
    assert_eq!(usage.input_tokens(), Some(12));
    assert_eq!(usage.output_tokens(), Some(9));
    assert_eq!(usage.total_tokens(), Some(21));
    assert_eq!(usage.cache_hit_tokens(), Some(4));
    assert_eq!(usage.cache_miss_tokens(), None);
    assert_eq!(usage.reasoning_tokens(), Some(2));
}

#[tokio::test]
async fn completes_through_injected_client() {
    // Arrange
    let model = ChatCompletionBackend::with_client(
        "stub-key".to_string(),
        "https://stub.example/v1/".to_string(),
        "qwen-stub".to_string(),
        policy("qwen-stub"),
        Arc::new(StubClient),
    );

    // Act
    let output = model
        .generate(&request("extract the name"))
        .await
        .expect("stubbed Qwen request should succeed");

    // Assert
    assert!(matches!(
        output,
        GeneratedResponse::Output { output, .. }
            if output == r#"{"name":"Ada"}"#
    ));
}

#[tokio::test]
async fn accepts_near_limit_escaped_structured_output() {
    // Arrange
    let server = MockServer::start().await;
    let empty_content =
        serde_json::to_string(&json!({ "value": "" })).expect("content should serialize");
    let value =
        "\\".repeat((schema_contract::RESPONSE_CONTENT_LIMIT_BYTES - empty_content.len()) / 2);
    let content =
        serde_json::to_string(&json!({ "value": value })).expect("content should serialize");
    let body = serde_json::to_vec(&json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": content}
        }]
    }))
    .expect("response should serialize");
    assert!(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES - content.len() <= 1);
    assert!(
        body.len() > schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + RESPONSE_ENVELOPE_LIMIT_BYTES
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(&server)
        .await;
    let model = qwen(&server);

    // Act
    let response = model
        .complete(model::ModelRequest::new(
            "return escaped content",
            escaped_value_schema(),
        ))
        .await
        .expect("near-limit escaped output should succeed");

    // Assert
    assert_eq!(
        response
            .output()
            .expect("response should contain terminal output")
            .get("value")
            .and_then(serde_json::Value::as_str)
            .map(str::len),
        Some(value.len())
    );
}

#[tokio::test]
async fn rejects_schemas_without_explicit_object_root() {
    // Arrange
    let server = MockServer::start().await;
    let model = qwen(&server);
    let schema_values = [
        json!({ "type": "array" }),
        json!({ "not": { "type": "object" } }),
        json!({
            "$defs": {
                "result": { "type": "object" }
            },
            "$ref": "#/$defs/result"
        }),
    ];

    // Act
    let mut errors = Vec::new();
    for schema_value in schema_values {
        let schema = OutputSchema::new(schema_value).expect("schema should be valid");
        errors.push(
            model
                .complete(model::ModelRequest::new("list names", schema))
                .await
                .expect_err("schema without an explicit object root should fail"),
        );
    }

    // Assert
    assert!(errors.into_iter().all(|error| matches!(
        error,
        model::ModelError::UnsupportedOutputSchema { reason }
            if reason == "Qwen JSON Object mode requires an explicit object root schema"
    )));
    assert!(
        server
            .received_requests()
            .await
            .expect("request recording should be enabled")
            .is_empty()
    );
}

#[tokio::test]
async fn rejects_malformed_structured_output() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": "not JSON"}
            }]
        })))
        .mount(&server)
        .await;
    let model = qwen(&server);

    // Act
    let error = model
        .complete(request("extract the name"))
        .await
        .expect_err("malformed JSON should fail");

    // Assert
    assert!(matches!(error, model::ModelError::InvalidJson { .. }));
}

#[tokio::test]
async fn rejects_structured_output_schema_violation() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":42}"#}
            }]
        })))
        .mount(&server)
        .await;
    let model = qwen(&server);

    // Act
    let error = model
        .complete(request("extract the name"))
        .await
        .expect_err("schema violation should fail");

    // Assert
    assert!(matches!(
        error,
        model::ModelError::SchemaViolation { path, reason }
            if path == "/name" && reason.contains("string")
    ));
}

#[tokio::test]
async fn rejects_successful_response_without_content() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": []
        })))
        .mount(&server)
        .await;
    let model = qwen(&server);

    // Act
    let error = model
        .complete(request("hello"))
        .await
        .expect_err("missing response content should fail");

    // Assert
    assert!(matches!(error, model::ModelError::InvalidResponse));
}
