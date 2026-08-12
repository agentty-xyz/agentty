use std::error::Error;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::provider::{self, KimiConfig, MuseConfig, QwenConfig};
use crate::schema_contract::{OutputSchema, OutputValidationError};
use crate::{chat_completion, telemetry, tool};

/// Object-safe boundary for provider-neutral model requests.
///
/// [`ModelClient`] implements this trait so applications can select supported
/// providers dynamically without exposing provider backends or raw generation.
#[async_trait]
pub trait Model: Send + Sync {
    /// Completes one model request.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}

/// Application-facing client for provider-neutral model requests.
///
/// Provider request execution remains private so every request passes through
/// [`ModelClient::complete`], which owns telemetry and structured-output
/// validation.
pub struct ModelClient {
    backend: chat_completion::ChatCompletionBackend,
    metadata: ModelMetadata,
}

impl ModelClient {
    /// Creates a client backed by Moonshot AI's Kimi API.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    pub fn kimi(config: KimiConfig) -> Result<Self, ModelMetadataError> {
        Self::chat_completion(
            config.api_key,
            config.base_url,
            config.model,
            provider::KIMI_POLICY,
        )
    }

    /// Creates a client backed by Meta's Model API for Muse models.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    pub fn muse(config: MuseConfig) -> Result<Self, ModelMetadataError> {
        Self::chat_completion(
            config.api_key,
            config.base_url,
            config.model,
            provider::MUSE_POLICY,
        )
    }

    /// Creates a client backed by Alibaba Cloud Model Studio's Qwen API.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    pub fn qwen(config: QwenConfig) -> Result<Self, ModelMetadataError> {
        Self::chat_completion(
            config.api_key,
            config.base_url,
            config.model,
            provider::QWEN_POLICY,
        )
    }

    /// Returns the validated provider and model identity retained by the
    /// client.
    pub fn metadata(&self) -> &ModelMetadata {
        &self.metadata
    }

    /// Completes one model request through the shared telemetry and
    /// structured-output lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    pub async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let _duration = telemetry::RequestDuration::start(self.metadata());
        let response = self.backend.generate(&request).await?;

        match response {
            chat_completion::GeneratedResponse::Output(output) => request
                .schema()
                .parse_and_validate(&output)
                .map(ModelResponse::from_output)
                .map_err(ModelError::from),
            chat_completion::GeneratedResponse::ToolCall(call) => {
                Ok(ModelResponse::tool_call(call))
            }
        }
    }

    fn chat_completion(
        api_key: String,
        base_url: String,
        model: String,
        policy: chat_completion::ChatCompletionProviderPolicy,
    ) -> Result<Self, ModelMetadataError> {
        let backend = chat_completion::ChatCompletionBackend::new(api_key, base_url, model, policy);
        let (provider, model) = backend.identity();
        let metadata = ModelMetadata::new(provider, model)?;

        Ok(Self { backend, metadata })
    }
}

#[async_trait]
impl Model for ModelClient {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        ModelClient::complete(self, request).await
    }
}

/// Validated provider and model identity used by the shared client lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelMetadata {
    model: String,
    provider: &'static str,
}

impl ModelMetadata {
    /// Creates metadata for one provider model.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when `provider` or `model` is empty or
    /// contains only whitespace.
    pub fn new(
        provider: &'static str,
        model: impl Into<String>,
    ) -> Result<Self, ModelMetadataError> {
        if provider.trim().is_empty() {
            return Err(ModelMetadataError::EmptyProvider);
        }
        let model = model.into();
        if model.trim().is_empty() {
            return Err(ModelMetadataError::EmptyModel);
        }

        Ok(Self { model, provider })
    }

    /// Returns the model identifier sent to the provider.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the provider identifier used by telemetry.
    pub fn provider(&self) -> &'static str {
        self.provider
    }
}

/// Invalid identity attributes supplied by a model provider.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ModelMetadataError {
    /// The provider identifier is empty or contains only whitespace.
    #[error("model provider must not be empty")]
    EmptyProvider,
    /// The model identifier is empty or contains only whitespace.
    #[error("model identifier must not be empty")]
    EmptyModel,
}

/// Provider-neutral input for one model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    prompt: String,
    schema: OutputSchema,
    tools: Vec<tool::ToolDefinition>,
}

impl ModelRequest {
    /// Creates a model request whose response must match `schema`.
    pub fn new(prompt: impl Into<String>, schema: OutputSchema) -> Self {
        Self {
            prompt: prompt.into(),
            schema,
            tools: Vec::new(),
        }
    }

    /// Advertises one native function tool for this request.
    #[must_use]
    pub fn with_tool(mut self, tool: tool::ToolDefinition) -> Self {
        self.tools.push(tool);

        self
    }

    /// Returns the request prompt.
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Returns the schema that the response must match.
    pub fn schema(&self) -> &OutputSchema {
        &self.schema
    }

    /// Returns the native function tools explicitly advertised by the caller.
    pub fn tools(&self) -> &[tool::ToolDefinition] {
        &self.tools
    }

    pub(crate) fn advertises_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name() == name)
    }
}

/// Provider-neutral output from one model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelResponse {
    /// Terminal, locally schema-validated model output.
    Output(Value),
    /// One validated native function call requiring application handling.
    ToolCall(tool::ToolCall),
}

impl ModelResponse {
    /// Returns parsed, schema-validated terminal output, when present.
    pub fn output(&self) -> Option<&Value> {
        match self {
            Self::Output(output) => Some(output),
            Self::ToolCall(_) => None,
        }
    }

    /// Returns the intermediate native function call, when present.
    pub fn call(&self) -> Option<&tool::ToolCall> {
        match self {
            Self::Output(_) => None,
            Self::ToolCall(call) => Some(call),
        }
    }

    fn from_output(output: Value) -> Self {
        Self::Output(output)
    }

    fn tool_call(call: tool::ToolCall) -> Self {
        Self::ToolCall(call)
    }
}

/// Failure returned while completing a model request.
#[derive(Debug, Error)]
pub enum ModelError {
    /// The provider request or response decoding failed.
    #[error("model request failed: {0}")]
    Request(#[source] Box<dyn Error + Send + Sync>),
    /// The provider returned a successful response without assistant content.
    #[error("model returned no response content")]
    InvalidResponse,
    /// The provider stopped before completing the model response.
    #[error("model response is incomplete: {reason}")]
    IncompleteResponse {
        /// Provider-specific reason generation stopped.
        reason: String,
    },
    /// The successful provider response body exceeds the adapter safety limit.
    #[error("model response body exceeds the size limit")]
    ResponseBodyTooLarge,
    /// The provider cannot represent the requested output schema.
    #[error("provider cannot satisfy this output schema: {reason}")]
    UnsupportedOutputSchema {
        /// Provider-specific reason the schema cannot be represented.
        reason: String,
    },
    /// The decoded provider response content exceeds the harness safety limit.
    #[error("model response content exceeds the size limit")]
    ResponseContentTooLarge,
    /// The provider returned malformed JSON for a structured request.
    #[error("model returned invalid JSON: {reason}")]
    InvalidJson {
        /// JSON parser diagnostic without the raw response body.
        reason: String,
    },
    /// The returned JSON does not conform to the requested schema.
    #[error("model output violates the schema at {path}: {reason}")]
    SchemaViolation {
        /// Bounded JSON Pointer-like path to the invalid value, or `$` for the
        /// root.
        path: String,
        /// Validator diagnostic for the failed constraint.
        reason: String,
    },
    /// The provider returned tool calls without any call entries.
    #[error("model returned no tool call")]
    MissingToolCall,
    /// The provider returned more than the single supported call.
    #[error("model returned multiple tool calls")]
    MultipleToolCalls,
    /// A tool-call response also contained terminal assistant content.
    #[error("model tool call response contained terminal content")]
    ToolCallWithContent,
    /// The provider returned an unsupported tool-call type.
    #[error("model requested unsupported tool type: {kind}")]
    UnsupportedToolType {
        /// Provider tool type that is not a native function.
        kind: String,
    },
    /// The provider returned an unsupported or unadvertised native function.
    #[error("model requested unsupported tool: {name}")]
    UnsupportedToolName {
        /// Native function name that was not advertised for the request.
        name: String,
    },
    /// The provider returned malformed or invalid native function arguments.
    #[error("model returned invalid tool arguments: {reason}")]
    InvalidToolArguments {
        /// Bounded parser or validation diagnostic.
        reason: String,
    },
}

impl ModelError {
    /// Wraps a provider transport or response-decoding failure.
    pub fn request(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Request(Box::new(error))
    }
}

impl From<OutputValidationError> for ModelError {
    fn from(error: OutputValidationError) -> Self {
        match error {
            OutputValidationError::InvalidJson(reason) => Self::InvalidJson { reason },
            OutputValidationError::SchemaViolation { path, reason } => {
                Self::SchemaViolation { path, reason }
            }
            OutputValidationError::TooLarge => Self::ResponseContentTooLarge,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use serde_json::json;

    use super::*;

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

        // Assert
        assert_eq!(metadata.provider(), "alibaba_cloud");
        assert_eq!(metadata.model(), "qwen-plus");
        assert_eq!(
            metadata,
            &ModelMetadata::new("alibaba_cloud", "qwen-plus").expect("metadata should be valid")
        );
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
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

        // Act
        let request = ModelRequest::new("hello", schema.clone());

        // Assert
        assert_eq!(request.prompt(), "hello");
        assert_eq!(request.schema(), &schema);
        assert!(request.tools().is_empty());
    }

    #[test]
    fn request_explicitly_advertises_read() {
        // Arrange
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

        // Act
        let request = ModelRequest::new("hello", schema).with_tool(tool::ToolDefinition::read());

        // Assert
        assert_eq!(request.tools(), &[tool::ToolDefinition::read()]);
        assert!(request.advertises_tool("read"));
        assert!(!request.advertises_tool("write"));
    }

    #[test]
    fn response_exposes_validated_output() {
        // Arrange
        let value = json!({ "name": "Ada" });

        // Act
        let response = ModelResponse::from_output(value.clone());

        // Assert
        assert_eq!(response.output(), Some(&value));
        assert!(response.call().is_none());
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
}
