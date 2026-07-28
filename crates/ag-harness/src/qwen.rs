use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{model, schema_contract};

const ERROR_BODY_LIMIT_BYTES: usize = 4 * 1024;
const JSON_STRING_MAX_EXPANSION: usize = 6;
const RESPONSE_ENVELOPE_LIMIT_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
const SUCCESS_BODY_LIMIT_BYTES: usize = schema_contract::RESPONSE_CONTENT_LIMIT_BYTES
    * JSON_STRING_MAX_EXPANSION
    + RESPONSE_ENVELOPE_LIMIT_BYTES;
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

/// Qwen implementation of the provider-neutral [`model::Model`] contract.
pub struct Qwen {
    client: reqwest::Client,
    config: QwenConfig,
}

impl Qwen {
    /// Creates a Qwen model adapter.
    pub fn new(config: QwenConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    async fn error_body_summary(
        response: &mut reqwest::Response,
    ) -> Result<String, reqwest::Error> {
        let mut body = Vec::new();
        let mut is_truncated = false;

        while let Some(chunk) = response.chunk().await? {
            let remaining = ERROR_BODY_LIMIT_BYTES.saturating_sub(body.len());

            if chunk.len() > remaining {
                body.extend_from_slice(&chunk[..remaining]);
                is_truncated = true;

                break;
            }

            body.extend_from_slice(&chunk);
        }

        let mut summary = String::from_utf8_lossy(&body)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if is_truncated {
            summary.push_str(" ...");
        }

        Ok(summary)
    }

    async fn success_body(response: &mut reqwest::Response) -> Result<Vec<u8>, model::ModelError> {
        let limit = u64::try_from(SUCCESS_BODY_LIMIT_BYTES).unwrap_or(u64::MAX);
        if response
            .content_length()
            .is_some_and(|content_length| content_length > limit)
        {
            return Err(model::ModelError::ResponseBodyTooLarge);
        }

        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(model::ModelError::request)? {
            Self::append_success_chunk(&mut body, &chunk)?;
        }

        Ok(body)
    }

    fn append_success_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), model::ModelError> {
        let remaining = SUCCESS_BODY_LIMIT_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            return Err(model::ModelError::ResponseBodyTooLarge);
        }

        body.extend_from_slice(chunk);

        Ok(())
    }
}

#[async_trait]
impl model::Model for Qwen {
    async fn complete(
        &self,
        request: model::ModelRequest,
    ) -> Result<model::ModelResponse, model::ModelError> {
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

        let mut response = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.config.api_key)
            .timeout(REQUEST_TIMEOUT)
            .json(&payload)
            .send()
            .await
            .map_err(model::ModelError::request)?;

        if let Err(source) = response.error_for_status_ref() {
            let body = Self::error_body_summary(&mut response)
                .await
                .map_err(model::ModelError::request)?;
            let error = QwenHttpError {
                body,
                source,
                status: response.status(),
            };

            return Err(model::ModelError::request(error));
        }

        let body = Self::success_body(&mut response).await?;
        let response =
            serde_json::from_slice::<QwenResponse>(&body).map_err(model::ModelError::request)?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or(model::ModelError::InvalidResponse)?;
        if choice.finish_reason != "stop" {
            return Err(model::ModelError::IncompleteResponse {
                reason: schema_contract::bounded_diagnostic(choice.finish_reason),
            });
        }
        let text = choice
            .message
            .content
            .ok_or(model::ModelError::InvalidResponse)?;

        request
            .schema()
            .parse_and_validate(&text)
            .map(model::ModelResponse::new)
            .map_err(model::ModelError::from)
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

#[derive(Deserialize)]
struct QwenResponse {
    choices: Vec<QwenChoice>,
}

#[derive(Deserialize)]
struct QwenChoice {
    finish_reason: String,
    message: QwenResponseMessage,
}

#[derive(Deserialize)]
struct QwenResponseMessage {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{bearer_token, body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::Model;

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

    #[test]
    fn rejects_success_chunk_that_exceeds_remaining_capacity() {
        // Arrange
        let mut body = vec![0; SUCCESS_BODY_LIMIT_BYTES - 1];

        // Act
        let error = Qwen::append_success_chunk(&mut body, &[0, 1])
            .expect_err("chunk exceeding the limit should fail");

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
