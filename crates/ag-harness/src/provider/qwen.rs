use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

use crate::{chat_completion, model, schema_contract};

const PROVIDER_NAME: &str = "alibaba_cloud";
const STRUCTURED_OUTPUT_INSTRUCTION: &str = concat!(
    "Return only one JSON object. The object must validate against this JSON Schema. ",
    "Do not include Markdown fences or any other text.\n\nJSON Schema:\n",
);
const UNSUPPORTED_SCHEMA_REASON: &str =
    "Qwen JSON Object mode requires an explicit object root schema";

/// Configuration for a Qwen model served through Alibaba Cloud Model Studio's
/// OpenAI-compatible API.
pub struct QwenConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the OpenAI-compatible version path.
    pub base_url: String,
    /// Qwen model identifier sent with each request.
    pub model: String,
}

/// Qwen implementation of the provider-neutral [`model::ModelBackend`]
/// strategy.
pub struct Qwen {
    client: Arc<dyn chat_completion::ChatCompletionClient>,
    config: QwenConfig,
}

impl Qwen {
    /// Creates a Qwen model adapter.
    pub fn new(config: QwenConfig) -> Self {
        Self::with_client(config, chat_completion::default_client())
    }

    fn with_client(
        config: QwenConfig,
        client: Arc<dyn chat_completion::ChatCompletionClient>,
    ) -> Self {
        Self { client, config }
    }

    fn map_completion_error(error: chat_completion::ChatCompletionError) -> model::ModelError {
        match error {
            chat_completion::ChatCompletionError::Http {
                body,
                source,
                status,
            } => model::ModelError::request(QwenHttpError {
                body,
                source,
                status,
            }),
            chat_completion::ChatCompletionError::ResponseBodyTooLarge => {
                model::ModelError::ResponseBodyTooLarge
            }
            error => model::ModelError::request(error),
        }
    }
}

#[async_trait]
impl model::ModelBackend for Qwen {
    fn metadata(&self) -> model::ModelMetadata<'_> {
        model::ModelMetadata::new(PROVIDER_NAME, &self.config.model)
    }

    async fn generate(&self, request: &model::ModelRequest) -> Result<String, model::ModelError> {
        if !request.schema().has_object_root() {
            return Err(model::ModelError::UnsupportedOutputSchema {
                reason: UNSUPPORTED_SCHEMA_REASON.to_string(),
            });
        }
        let messages = vec![
            QwenMessage {
                content: format!(
                    "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                    request.schema().value()
                ),
                role: "system",
            },
            QwenMessage {
                content: request.prompt().to_string(),
                role: "user",
            },
        ];
        let payload = QwenRequest {
            messages,
            model: &self.config.model,
            response_format: QwenResponseFormat {
                kind: "json_object",
            },
        };
        let payload = serde_json::to_value(payload).map_err(model::ModelError::request)?;
        let completion = self
            .client
            .complete(chat_completion::ChatCompletionRequest::new(
                &self.config.api_key,
                chat_completion::endpoint(&self.config.base_url),
                payload,
            ))
            .await
            .map_err(Self::map_completion_error)?
            .ok_or(model::ModelError::InvalidResponse)?;
        let (finish_reason, content) = completion.into_parts();
        if finish_reason != "stop" {
            return Err(model::ModelError::IncompleteResponse {
                reason: schema_contract::bounded_diagnostic(finish_reason),
            });
        }
        let text = content.ok_or(model::ModelError::InvalidResponse)?;

        Ok(text)
    }
}

#[derive(Debug, Error)]
#[error("Qwen returned HTTP {status}: {body}")]
struct QwenHttpError {
    body: String,
    #[source]
    source: reqwest::Error,
    status: reqwest::StatusCode,
}

#[derive(Serialize)]
struct QwenRequest<'a> {
    messages: Vec<QwenMessage>,
    model: &'a str,
    response_format: QwenResponseFormat,
}

#[derive(Serialize)]
struct QwenMessage {
    content: String,
    role: &'static str,
}

#[derive(Serialize)]
struct QwenResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{bearer_token, body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::Model;
    use crate::chat_completion::{
        ChatCompletion, ChatCompletionClient, ChatCompletionError, ChatCompletionRequest,
        ERROR_BODY_LIMIT_BYTES, RESPONSE_ENVELOPE_LIMIT_BYTES, SUCCESS_BODY_LIMIT_BYTES,
    };

    struct StubClient;

    #[async_trait]
    impl ChatCompletionClient for StubClient {
        async fn complete(
            &self,
            request: ChatCompletionRequest<'_>,
        ) -> Result<Option<ChatCompletion>, ChatCompletionError> {
            let (api_key, endpoint, payload) = request.into_parts();
            assert_eq!(api_key, "stub-key");
            assert_eq!(endpoint, "https://stub.example/v1/chat/completions");
            assert_eq!(payload["model"], "qwen-stub");
            assert_eq!(payload["response_format"]["type"], "json_object");

            Ok(Some(ChatCompletion::new(
                "stop".to_string(),
                Some(r#"{"name":"Ada"}"#.to_string()),
            )))
        }
    }

    fn person_schema_value() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    fn person_schema() -> crate::OutputSchema {
        crate::OutputSchema::new(person_schema_value()).expect("schema should be valid")
    }

    fn request(prompt: &str) -> model::ModelRequest {
        model::ModelRequest::new(prompt, person_schema())
    }

    fn escaped_value_schema() -> crate::OutputSchema {
        crate::OutputSchema::new(json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" }
            },
            "required": ["value"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn qwen(server: &MockServer) -> Qwen {
        Qwen::new(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: format!("{}/", server.uri()),
            model: "qwen-plus".to_string(),
        })
    }

    async fn mount_structured_response(
        server: &MockServer,
        prompt: &str,
        schema: &serde_json::Value,
        content: &str,
    ) {
        let schema_instruction = format!("{STRUCTURED_OUTPUT_INSTRUCTION}{schema}");
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {"content": schema_instruction, "role": "system"},
                    {"content": prompt, "role": "user"}
                ],
                "model": "qwen-plus",
                "response_format": {"type": "json_object"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": content}
                }]
            })))
            .expect(1)
            .mount(server)
            .await;
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
        let model = qwen(&server);

        // Act
        let response = model
            .complete(request("extract the name"))
            .await
            .expect("Qwen request should succeed");

        // Assert
        assert_eq!(response.output(), &json!({ "name": "Ada" }));
    }

    #[tokio::test]
    async fn completes_through_injected_client() {
        // Arrange
        let model = Qwen::with_client(
            QwenConfig {
                api_key: "stub-key".to_string(),
                base_url: "https://stub.example/v1/".to_string(),
                model: "qwen-stub".to_string(),
            },
            Arc::new(StubClient),
        );

        // Act
        let response = model
            .complete(request("extract the name"))
            .await
            .expect("stubbed Qwen request should succeed");

        // Assert
        assert_eq!(response.output(), &json!({ "name": "Ada" }));
    }

    #[tokio::test]
    async fn rejects_structured_response_stopped_for_length() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "length",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("truncated response should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::IncompleteResponse { reason } if reason == "length"
        ));
    }

    #[tokio::test]
    async fn bounds_incomplete_response_reason() {
        // Arrange
        let server = MockServer::start().await;
        let finish_reason = "x".repeat(1024);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": finish_reason.clone(),
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("incomplete response should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::IncompleteResponse { reason }
                if reason == schema_contract::bounded_diagnostic(finish_reason)
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
            body.len()
                > schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + RESPONSE_ENVELOPE_LIMIT_BYTES
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
                .get("value")
                .and_then(serde_json::Value::as_str)
                .map(str::len),
            Some(value.len())
        );
    }

    #[tokio::test]
    async fn rejects_oversized_success_body_before_decoding() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                b'x';
                SUCCESS_BODY_LIMIT_BYTES
                    + 1
            ]))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("oversized successful response should fail");

        // Assert
        assert!(matches!(error, model::ModelError::ResponseBodyTooLarge));
    }

    #[tokio::test]
    async fn rejects_oversized_response_content() {
        // Arrange
        let server = MockServer::start().await;
        let content = "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": content}
                }]
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("oversized response content should fail");

        // Assert
        assert!(matches!(error, model::ModelError::ResponseContentTooLarge));
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
            let schema = crate::OutputSchema::new(schema_value).expect("schema should be valid");
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
                if reason == UNSUPPORTED_SCHEMA_REASON
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

    #[tokio::test]
    async fn returns_request_error_for_http_failure() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "invalid API key"}
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("HTTP failure should fail");

        // Assert
        assert_eq!(
            error.to_string(),
            "model request failed: Qwen returned HTTP 401 Unauthorized: \
             {\"error\":{\"message\":\"invalid API key\"}}"
        );
        let provider_error = std::error::Error::source(&error)
            .expect("HTTP failure should retain its provider error");
        let source = provider_error
            .source()
            .and_then(|source| source.downcast_ref::<reqwest::Error>())
            .expect("HTTP failure should retain its reqwest source");
        assert_eq!(source.status(), Some(reqwest::StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn bounds_http_error_body() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(500).set_body_string("x".repeat(ERROR_BODY_LIMIT_BYTES + 1)),
            )
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("HTTP failure should fail");
        let message = error.to_string();

        // Assert
        assert_eq!(
            message,
            format!(
                "model request failed: Qwen returned HTTP 500 Internal Server Error: {} ...",
                "x".repeat(ERROR_BODY_LIMIT_BYTES)
            )
        );
    }

    #[tokio::test]
    async fn returns_request_error_for_malformed_response() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("malformed response should fail");

        // Assert
        assert!(matches!(error, model::ModelError::Request(_)));
    }
}
