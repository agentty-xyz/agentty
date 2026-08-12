use std::error::Error;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{model, schema_contract, tool};

pub(crate) const ERROR_BODY_LIMIT_BYTES: usize = 4 * 1024;
const JSON_STRING_MAX_EXPANSION: usize = 6;
pub(crate) const RESPONSE_ENVELOPE_LIMIT_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
pub(crate) const SUCCESS_BODY_LIMIT_BYTES: usize = schema_contract::RESPONSE_CONTENT_LIMIT_BYTES
    * JSON_STRING_MAX_EXPANSION
    + RESPONSE_ENVELOPE_LIMIT_BYTES;
pub(crate) const STRUCTURED_OUTPUT_INSTRUCTION: &str = concat!(
    "Return only one JSON object. The object must validate against this JSON Schema. ",
    "Do not include Markdown fences or any other text.\n\nJSON Schema:\n",
);

/// Provider policy applied by the shared JSON Object backend.
#[derive(Clone, Copy)]
pub(crate) struct JsonObjectProviderPolicy {
    pub(crate) display_name: &'static str,
    pub(crate) telemetry_name: &'static str,
    pub(crate) unsupported_schema_reason: &'static str,
}

/// Provider-neutral result of decoding one Chat Completions choice.
pub(crate) enum GeneratedResponse {
    Output(String),
    ToolCall(tool::ToolCall),
}

/// Shared JSON Object backend for OpenAI-compatible Chat Completions APIs.
pub(crate) struct JsonObjectBackend {
    api_key: String,
    base_url: String,
    client: Arc<dyn ChatCompletionClient>,
    model: String,
    policy: JsonObjectProviderPolicy,
}

impl JsonObjectBackend {
    /// Creates a JSON Object backend with the production HTTP client.
    pub(crate) fn new(
        api_key: String,
        base_url: String,
        model: String,
        policy: JsonObjectProviderPolicy,
    ) -> Self {
        Self::with_client(api_key, base_url, model, policy, default_client())
    }

    /// Returns the backend's telemetry identity.
    pub(crate) fn identity(&self) -> (&'static str, &str) {
        (self.policy.telemetry_name, &self.model)
    }

    /// Generates raw JSON Object output through the shared wire lifecycle.
    pub(crate) async fn generate(
        &self,
        request: &model::ModelRequest,
    ) -> Result<GeneratedResponse, model::ModelError> {
        if !request.schema().has_object_root() {
            return Err(model::ModelError::UnsupportedOutputSchema {
                reason: self.policy.unsupported_schema_reason.to_string(),
            });
        }
        let messages = vec![
            JsonObjectMessage {
                content: format!(
                    "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                    request.schema().value()
                ),
                role: "system",
            },
            JsonObjectMessage {
                content: request.prompt().to_string(),
                role: "user",
            },
        ];
        let payload = JsonObjectRequest {
            messages,
            model: &self.model,
            response_format: JsonObjectResponseFormat {
                kind: "json_object",
            },
            tools: request.tools().iter().map(JsonObjectTool::from).collect(),
        };
        let payload = serde_json::to_value(payload).map_err(model::ModelError::request)?;
        let completion = self
            .client
            .complete(ChatCompletionRequest::new(
                &self.api_key,
                endpoint(&self.base_url),
                payload,
            ))
            .await
            .map_err(|error| self.map_completion_error(error))?
            .ok_or(model::ModelError::InvalidResponse)?;
        let (finish_reason, content, tool_calls) = completion.into_parts();
        match finish_reason.as_str() {
            "stop" => content
                .map(GeneratedResponse::Output)
                .ok_or(model::ModelError::InvalidResponse),
            "tool_calls" => Self::decode_tool_call(request, content.as_deref(), tool_calls),
            _ => Err(model::ModelError::IncompleteResponse {
                reason: schema_contract::bounded_diagnostic(finish_reason),
            }),
        }
    }

    /// Creates a JSON Object backend with an injected transport client.
    pub(crate) fn with_client(
        api_key: String,
        base_url: String,
        model: String,
        policy: JsonObjectProviderPolicy,
        client: Arc<dyn ChatCompletionClient>,
    ) -> Self {
        Self {
            api_key,
            base_url,
            client,
            model,
            policy,
        }
    }

    fn map_completion_error(&self, error: ChatCompletionError) -> model::ModelError {
        match error {
            ChatCompletionError::Http {
                body,
                source,
                status,
            } => model::ModelError::request(ProviderHttpError {
                body,
                provider: self.policy.display_name,
                source,
                status,
            }),
            ChatCompletionError::ResponseBodyTooLarge => model::ModelError::ResponseBodyTooLarge,
            error => model::ModelError::request(error),
        }
    }

    fn decode_tool_call(
        request: &model::ModelRequest,
        content: Option<&str>,
        mut calls: Vec<ChatCompletionToolCall>,
    ) -> Result<GeneratedResponse, model::ModelError> {
        if content.is_some_and(|content| !content.is_empty()) {
            return Err(model::ModelError::ToolCallWithContent);
        }
        let call = match calls.len() {
            0 => return Err(model::ModelError::MissingToolCall),
            1 => calls.remove(0),
            _ => return Err(model::ModelError::MultipleToolCalls),
        };
        if call.kind != "function" {
            return Err(model::ModelError::UnsupportedToolType {
                kind: schema_contract::bounded_diagnostic(call.kind),
            });
        }
        let function = serde_json::from_value::<ChatCompletionFunctionCall>(call.function)
            .map_err(|error| model::ModelError::InvalidToolArguments {
                reason: schema_contract::bounded_diagnostic(error),
            })?;
        if !request.advertises_tool(&function.name) {
            return Err(model::ModelError::UnsupportedToolName {
                name: schema_contract::bounded_diagnostic(function.name),
            });
        }
        schema_contract::ensure_content_size(&function.arguments)
            .map_err(model::ModelError::from)?;
        let arguments =
            serde_json::from_str::<tool::ReadArguments>(&function.arguments).map_err(|error| {
                model::ModelError::InvalidToolArguments {
                    reason: schema_contract::bounded_diagnostic(error),
                }
            })?;

        Ok(GeneratedResponse::ToolCall(tool::ToolCall::read(
            call.id, arguments,
        )))
    }
}

/// One provider-authenticated request using the Chat Completions wire API.
pub(crate) struct ChatCompletionRequest<'a> {
    api_key: &'a str,
    endpoint: String,
    payload: Value,
}

impl<'a> ChatCompletionRequest<'a> {
    /// Creates a request from provider-owned authentication and payload data.
    pub(crate) fn new(api_key: &'a str, endpoint: String, payload: Value) -> Self {
        Self {
            api_key,
            endpoint,
            payload,
        }
    }

    /// Consumes the request into values usable by a client implementation.
    pub(crate) fn into_parts(self) -> (&'a str, String, Value) {
        (self.api_key, self.endpoint, self.payload)
    }
}

/// Provider-independent fields extracted from the first completion choice.
pub(crate) struct ChatCompletion {
    content: Option<String>,
    finish_reason: String,
    tool_calls: Vec<ChatCompletionToolCall>,
}

impl ChatCompletion {
    /// Creates one normalized completion choice.
    pub(crate) fn new(finish_reason: String, content: Option<String>) -> Self {
        Self {
            content,
            finish_reason,
            tool_calls: Vec::new(),
        }
    }

    /// Consumes the choice into provider-interpreted completion fields.
    fn into_parts(self) -> (String, Option<String>, Vec<ChatCompletionToolCall>) {
        (self.finish_reason, self.content, self.tool_calls)
    }
}

/// Decoded client boundary between provider adapters and the Chat Completions
/// API.
#[async_trait]
pub(crate) trait ChatCompletionClient: Send + Sync {
    async fn complete(
        &self,
        request: ChatCompletionRequest<'_>,
    ) -> Result<Option<ChatCompletion>, ChatCompletionError>;
}

/// Builds the Chat Completions endpoint for a provider base URL.
pub(crate) fn endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

/// Creates the production Chat Completions client implementation.
pub(crate) fn default_client() -> Arc<dyn ChatCompletionClient> {
    Arc::new(ReqwestChatCompletionClient {
        client: reqwest::Client::new(),
    })
}

struct ReqwestChatCompletionClient {
    client: reqwest::Client,
}

#[async_trait]
impl ChatCompletionClient for ReqwestChatCompletionClient {
    async fn complete(
        &self,
        request: ChatCompletionRequest<'_>,
    ) -> Result<Option<ChatCompletion>, ChatCompletionError> {
        let (api_key, endpoint, payload) = request.into_parts();
        let mut response = self
            .client
            .post(endpoint)
            .bearer_auth(api_key)
            .timeout(REQUEST_TIMEOUT)
            .json(&payload)
            .send()
            .await
            .map_err(ChatCompletionError::transport)?;

        if let Err(source) = response.error_for_status_ref() {
            let status = response.status();
            let body = error_body_summary(&mut response).await;

            return Err(ChatCompletionError::Http {
                body,
                source,
                status,
            });
        }

        let body = success_body(&mut response).await?;
        let response = serde_json::from_slice::<ChatCompletionResponse>(&body)
            .map_err(ChatCompletionError::InvalidResponse)?;

        Ok(response.choices.into_iter().next().map(|choice| {
            let mut completion = ChatCompletion::new(choice.finish_reason, choice.message.content);
            completion.tool_calls = choice.message.tool_calls.unwrap_or_default();

            completion
        }))
    }
}

async fn error_body_summary(response: &mut reqwest::Response) -> String {
    let mut body = Vec::new();
    let mut is_truncated = false;
    let read_error = loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break None,
            Err(error) => break Some(error),
        };

        let remaining = ERROR_BODY_LIMIT_BYTES.saturating_sub(body.len());

        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            is_truncated = true;

            break None;
        }

        body.extend_from_slice(&chunk);
    };

    let mut summary = String::from_utf8_lossy(&body)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if is_truncated {
        summary.push_str(" ...");
    }
    if let Some(error) = read_error {
        if !summary.is_empty() {
            summary.push(' ');
        }
        let _ = write!(&mut summary, "[error body read failed: {error}]");
    }

    summary
}

async fn success_body(response: &mut reqwest::Response) -> Result<Vec<u8>, ChatCompletionError> {
    let limit = u64::try_from(SUCCESS_BODY_LIMIT_BYTES).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|content_length| content_length > limit)
    {
        return Err(ChatCompletionError::ResponseBodyTooLarge);
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(ChatCompletionError::transport)?
    {
        append_success_chunk(&mut body, &chunk)?;
    }

    Ok(body)
}

fn append_success_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), ChatCompletionError> {
    let remaining = SUCCESS_BODY_LIMIT_BYTES.saturating_sub(body.len());
    if chunk.len() > remaining {
        return Err(ChatCompletionError::ResponseBodyTooLarge);
    }

    body.extend_from_slice(chunk);

    Ok(())
}

/// Failure produced by a Chat Completions client implementation.
#[derive(Debug, Error)]
pub(crate) enum ChatCompletionError {
    #[error("Chat Completions returned HTTP {status}: {body}")]
    Http {
        body: String,
        #[source]
        source: reqwest::Error,
        status: reqwest::StatusCode,
    },
    #[error("Chat Completions returned an invalid response: {0}")]
    InvalidResponse(#[source] serde_json::Error),
    #[error("Chat Completions response body exceeds the size limit")]
    ResponseBodyTooLarge,
    #[error("Chat Completions transport failed: {0}")]
    Transport(#[source] Box<dyn Error + Send + Sync>),
}

impl ChatCompletionError {
    fn transport(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Transport(Box::new(error))
    }
}

#[derive(Debug, Error)]
#[error("{provider} returned HTTP {status}: {body}")]
struct ProviderHttpError {
    body: String,
    provider: &'static str,
    #[source]
    source: reqwest::Error,
    status: reqwest::StatusCode,
}

#[derive(Serialize)]
struct JsonObjectRequest<'a> {
    messages: Vec<JsonObjectMessage>,
    model: &'a str,
    response_format: JsonObjectResponseFormat,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<JsonObjectTool<'a>>,
}

#[derive(Serialize)]
struct JsonObjectMessage {
    content: String,
    role: &'static str,
}

#[derive(Serialize)]
struct JsonObjectResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct JsonObjectTool<'a> {
    function: JsonObjectFunction<'a>,
    #[serde(rename = "type")]
    kind: &'static str,
}

impl<'a> From<&'a tool::ToolDefinition> for JsonObjectTool<'a> {
    fn from(definition: &'a tool::ToolDefinition) -> Self {
        Self {
            function: JsonObjectFunction {
                description: definition.description(),
                name: definition.name(),
                parameters: definition.parameters(),
            },
            kind: "function",
        }
    }
}

#[derive(Serialize)]
struct JsonObjectFunction<'a> {
    description: &'static str,
    name: &'static str,
    parameters: &'a Value,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    finish_reason: String,
    message: ChatCompletionMessage,
}

#[derive(Deserialize)]
struct ChatCompletionMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatCompletionToolCall>>,
}

#[derive(Deserialize)]
struct ChatCompletionToolCall {
    #[serde(default)]
    function: Value,
    id: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ChatCompletionFunctionCall {
    arguments: String,
    name: String,
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn builds_endpoint_from_base_url() {
        // Arrange and Act
        let endpoint = endpoint("https://example.com/v1///");

        // Assert
        assert_eq!(endpoint, "https://example.com/v1/chat/completions");
    }

    #[test]
    fn rejects_success_chunk_that_exceeds_remaining_capacity() {
        // Arrange
        let mut body = vec![0; SUCCESS_BODY_LIMIT_BYTES - 1];

        // Act
        let error = append_success_chunk(&mut body, &[0, 1])
            .expect_err("chunk exceeding the limit should fail");

        // Assert
        assert!(matches!(error, ChatCompletionError::ResponseBodyTooLarge));
    }

    #[test]
    fn wraps_transport_error_with_its_source() {
        // Arrange
        let source = io::Error::other("connection reset");

        // Act
        let error = ChatCompletionError::transport(source);

        // Assert
        assert_eq!(
            error.to_string(),
            "Chat Completions transport failed: connection reset"
        );
        assert_eq!(
            std::error::Error::source(&error)
                .expect("transport failure should retain its source")
                .to_string(),
            "connection reset"
        );
    }

    #[tokio::test]
    async fn retains_http_status_when_error_body_read_fails() {
        // Arrange
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("truncated-response listener should bind");
        let address = listener
            .local_addr()
            .expect("truncated-response listener should have an address");
        let server = tokio::task::spawn_blocking(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("truncated-response listener should accept a request");
            let mut request = [0; 2_048];
            let bytes_read = stream
                .read(&mut request)
                .expect("truncated-response server should read the request");
            assert!(
                bytes_read > 0,
                "truncated-response request should not be empty"
            );
            stream
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\n\
                      Content-Length: 64\r\n\
                      Connection: close\r\n\r\n\
                      partial error body",
                )
                .expect("truncated-response server should write the response");
        });
        let client = ReqwestChatCompletionClient {
            client: reqwest::Client::new(),
        };
        let request = ChatCompletionRequest::new(
            "test-key",
            format!("http://{address}"),
            serde_json::json!({}),
        );

        // Act
        let result = client.complete(request).await;
        server
            .await
            .expect("truncated-response server should finish");

        // Assert
        assert!(result.is_err(), "truncated HTTP error response should fail");
        let error = result
            .err()
            .expect("truncated HTTP error response should contain an error");
        assert!(matches!(
            &error,
            ChatCompletionError::Http { status, .. }
                if *status == reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(error.to_string().contains("error body read failed"));
        let source = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<reqwest::Error>())
            .expect("HTTP failure should retain its status-bearing source");
        assert_eq!(
            source.status(),
            Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
        );
    }
}
