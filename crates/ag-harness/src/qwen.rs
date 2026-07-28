use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model;

const ERROR_BODY_LIMIT_BYTES: usize = 4 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

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
}

#[async_trait]
impl model::Model for Qwen {
    async fn complete(
        &self,
        request: model::ModelRequest,
    ) -> Result<model::ModelResponse, model::ModelError> {
        let payload = QwenRequest {
            messages: [QwenMessage {
                content: request.prompt(),
                role: "user",
            }],
            model: &self.config.model,
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

        let response = response
            .json::<QwenResponse>()
            .await
            .map_err(model::ModelError::request)?;

        let text = response
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or(model::ModelError::InvalidResponse)?;

        Ok(model::ModelResponse::from_text(text))
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
    messages: [QwenMessage<'a>; 1],
    model: &'a str,
}

#[derive(Serialize)]
struct QwenMessage<'a> {
    content: &'a str,
    role: &'static str,
}

#[derive(Deserialize)]
struct QwenResponse {
    choices: Vec<QwenChoice>,
}

#[derive(Deserialize)]
struct QwenChoice {
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

    fn qwen(server: &MockServer) -> Qwen {
        Qwen::new(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: format!("{}/", server.uri()),
            model: "qwen-plus".to_string(),
        })
    }

    #[tokio::test]
    async fn completes_text_request() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [{"content": "hello", "role": "user"}],
                "model": "qwen-plus"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "Hello!"}}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let response = model
            .complete(model::ModelRequest::text("hello"))
            .await
            .expect("Qwen request should succeed");

        // Assert
        assert_eq!(response.text(), "Hello!");
    }

    #[tokio::test]
    async fn rejects_successful_response_without_text() {
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
            .complete(model::ModelRequest::text("hello"))
            .await
            .expect_err("missing response text should fail");

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
            .complete(model::ModelRequest::text("hello"))
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
            .complete(model::ModelRequest::text("hello"))
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
            .complete(model::ModelRequest::text("hello"))
            .await
            .expect_err("malformed response should fail");

        // Assert
        assert!(matches!(error, model::ModelError::Request(_)));
    }
}
