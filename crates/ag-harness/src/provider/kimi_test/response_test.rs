use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{
    escaped_value_schema, kimi, mount_structured_response, person_schema_value, request,
};
use crate::chat_completion::RESPONSE_ENVELOPE_LIMIT_BYTES;
use crate::provider::kimi::KimiConfig;
use crate::schema_contract::OutputSchema;
use crate::{model, schema_contract};

#[test]
fn metadata_exposes_provider_and_model() {
    // Arrange
    let model = model::ModelClient::kimi(KimiConfig {
        api_key: "test-key".to_string(),
        base_url: "https://api.moonshot.example/v1".to_string(),
        model: "kimi-k2.6".to_string(),
    })
    .expect("fixture configuration should be valid");

    // Act
    let metadata = model.metadata();

    // Assert
    assert_eq!(metadata.provider(), "moonshot_ai");
    assert_eq!(metadata.model(), "kimi-k2.6");
}

#[test]
fn rejects_empty_model_during_construction() {
    // Arrange
    let config = KimiConfig {
        api_key: "test-key".to_string(),
        base_url: "https://api.moonshot.example/v1".to_string(),
        model: "  ".to_string(),
    };

    // Act
    let error = model::ModelClient::kimi(config)
        .err()
        .expect("empty model configuration should be rejected");

    // Assert
    assert_eq!(error, model::ModelMetadataError::EmptyModel);
}

#[tokio::test]
async fn accepts_object_root_in_type_array() {
    // Arrange
    let server = MockServer::start().await;
    let schema_value = json!({ "type": ["object"] });
    mount_structured_response(&server, "return an object", &schema_value, "{}").await;
    let model = kimi(&server);
    let schema = OutputSchema::new(schema_value).expect("schema should be valid");

    // Act
    let response = model
        .complete(model::ModelRequest::new("return an object", schema))
        .await
        .expect("Kimi request should succeed");

    // Assert
    assert_eq!(response.output(), Some(&json!({})));
}

#[tokio::test]
async fn completes_structured_request() {
    // Arrange
    let server = MockServer::start().await;
    let schema_value = person_schema_value();
    mount_structured_response(
        &server,
        "extract the name",
        &schema_value,
        r#"{"name":"Ada"}"#,
    )
    .await;
    let model = kimi(&server);

    // Act
    let response = model
        .complete(request("extract the name"))
        .await
        .expect("Kimi request should succeed");

    // Assert
    assert_eq!(response.output(), Some(&json!({ "name": "Ada" })));
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
    let model = kimi(&server);

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
            .and_then(Value::as_str)
            .map(str::len),
        Some(value.len())
    );
}

#[tokio::test]
async fn rejects_schemas_without_explicit_object_root() {
    // Arrange
    let server = MockServer::start().await;
    let model = kimi(&server);
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
    assert!(
        errors
            .into_iter()
            .all(|error| matches!(error, model::ModelError::UnsupportedOutputSchema { .. }))
    );
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
    let model = kimi(&server);

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
    let model = kimi(&server);

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
async fn rejects_successful_response_without_choices() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": []
        })))
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(request("hello"))
        .await
        .expect_err("missing response choice should fail");

    // Assert
    assert!(matches!(error, model::ModelError::InvalidResponse));
}

#[tokio::test]
async fn rejects_successful_response_without_content() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": null}
            }]
        })))
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(request("hello"))
        .await
        .expect_err("missing response content should fail");

    // Assert
    assert!(matches!(error, model::ModelError::InvalidResponse));
}
