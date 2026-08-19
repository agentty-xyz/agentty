use std::error::Error;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::lifecycle::{LifecycleEmitter, LifecycleObserver, ModelResponseType};
use crate::provider::{self, KimiConfig, MuseConfig, QwenConfig};
use crate::schema_contract::{OutputSchema, OutputValidationError};
use crate::{chat_completion, telemetry, tool};

/// Object-safe boundary for provider-neutral model requests.
///
/// [`ModelClient`] implements this trait so applications can select supported
/// providers dynamically without exposing provider backends or raw generation.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Model: Send + Sync {
    /// Completes one model request.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;

    /// Completes one model request and returns normalized metadata when
    /// available.
    ///
    /// Response-only implementations inherit a default that returns `None`.
    /// [`ModelWithMetadata`] implementations return `Some` automatically.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] under the same conditions as [`Model::complete`].
    async fn complete_with_optional_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<(ModelResponse, Option<CompletionMetadata>), ModelError> {
        self.complete(request)
            .await
            .map(|response| (response, None))
    }
}

/// Object-safe model boundary that guarantees normalized completion metadata.
#[async_trait]
pub trait ModelWithMetadata: Send + Sync {
    /// Completes one model request and returns normalized provider metadata.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    async fn complete_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelCompletion, ModelError>;
}

#[async_trait]
impl<ModelType> Model for ModelType
where
    ModelType: ModelWithMetadata + ?Sized,
{
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.complete_with_metadata(request)
            .await
            .map(ModelCompletion::into_response)
    }

    async fn complete_with_optional_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<(ModelResponse, Option<CompletionMetadata>), ModelError> {
        let completion = self.complete_with_metadata(request).await?;
        let ModelCompletion { metadata, response } = completion;

        Ok((response, Some(metadata)))
    }
}

/// Application-facing client for provider-neutral model requests.
///
/// Provider request execution remains private so every request passes through
/// [`ModelClient::complete`], which owns telemetry and structured-output
/// validation.
pub struct ModelClient {
    backend: chat_completion::ChatCompletionBackend,
    lifecycle: LifecycleEmitter,
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

    /// Sends metadata-only request lifecycle events to `observer`.
    #[must_use]
    pub fn with_lifecycle_observer(mut self, observer: impl LifecycleObserver + 'static) -> Self {
        self.lifecycle = LifecycleEmitter::new(observer);

        self
    }

    /// Completes one model request through the shared telemetry and
    /// structured-output lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    pub async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.complete_with_metadata(request)
            .await
            .map(ModelCompletion::into_response)
    }

    /// Completes one model request and returns normalized provider metadata.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    pub async fn complete_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelCompletion, ModelError> {
        let metrics = telemetry::RequestMetrics::start(self.metadata());
        let lifecycle = if request.lifecycle_observed() {
            None
        } else {
            self.lifecycle
                .start_model_request(Some(self.metadata.clone()), 0, None)
        };
        let (result, failure_metadata) = match self.backend.generate(&request).await {
            Ok(chat_completion::GeneratedResponse::Failed { error, metadata }) => {
                (Err(error), Some(metadata))
            }
            Ok(chat_completion::GeneratedResponse::Output { metadata, output }) => {
                match request.schema().parse_and_validate(&output) {
                    Ok(response) => (
                        Ok(ModelCompletion::new(
                            metadata,
                            ModelResponse::from_output(response),
                        )),
                        None,
                    ),
                    Err(error) => (Err(ModelError::from(error)), Some(metadata)),
                }
            }
            Ok(chat_completion::GeneratedResponse::ToolCall { call, metadata }) => (
                Ok(ModelCompletion::new(
                    metadata,
                    ModelResponse::tool_call(call),
                )),
                None,
            ),
            Err(error) => (Err(error), None),
        };

        match &result {
            Ok(completion) => metrics.completed(completion.metadata()),
            Err(error) => metrics.failed(error, failure_metadata.as_ref()),
        }

        if let Some(lifecycle) = lifecycle {
            match &result {
                Ok(completion) => lifecycle.completed(
                    Some(completion.metadata.clone()),
                    completion.response.response_type(),
                ),
                Err(error) => lifecycle.failed(error.error_type()),
            }
        }

        result
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

        Ok(Self {
            backend,
            lifecycle: LifecycleEmitter::default(),
            metadata,
        })
    }
}

#[async_trait]
impl ModelWithMetadata for ModelClient {
    async fn complete_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelCompletion, ModelError> {
        ModelClient::complete_with_metadata(self, request).await
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
    lifecycle_observed: bool,
    messages: Vec<ModelMessage>,
    prompt: String,
    schema: OutputSchema,
    tools: Vec<tool::ToolDefinition>,
}

impl ModelRequest {
    /// Creates a model request whose response must match `schema`.
    pub fn new(prompt: impl Into<String>, schema: OutputSchema) -> Self {
        let prompt = prompt.into();

        Self {
            lifecycle_observed: false,
            messages: vec![ModelMessage::User(prompt.clone())],
            prompt,
            schema,
            tools: Vec::new(),
        }
    }

    /// Advertises one native function tool for this request.
    #[must_use]
    pub fn with_tool(mut self, tool: tool::ToolDefinition) -> Self {
        if !self.advertises_tool(tool.name()) {
            self.tools.push(tool);
        }

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

    pub(crate) fn messages(&self) -> &[ModelMessage] {
        &self.messages
    }

    pub(crate) fn lifecycle_observed(&self) -> bool {
        self.lifecycle_observed
    }

    pub(crate) fn mark_lifecycle_observed(&mut self) {
        self.lifecycle_observed = true;
    }

    pub(crate) fn record_tool_result(&mut self, call: tool::ToolCall, content: String) {
        let call_id = call.id().to_string();
        let name = call.name().to_string();
        self.messages.push(ModelMessage::AssistantToolCall(call));
        self.messages.push(ModelMessage::ToolResult {
            call_id,
            content,
            name,
        });
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ModelMessage {
    User(String),
    AssistantToolCall(tool::ToolCall),
    ToolResult {
        call_id: String,
        content: String,
        name: String,
    },
}

/// One model response paired with normalized provider completion metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCompletion {
    metadata: CompletionMetadata,
    response: ModelResponse,
}

impl ModelCompletion {
    /// Creates a completion from normalized metadata and a model response.
    pub fn new(metadata: CompletionMetadata, response: ModelResponse) -> Self {
        Self { metadata, response }
    }

    /// Returns the normalized metadata reported by the provider.
    pub fn metadata(&self) -> &CompletionMetadata {
        &self.metadata
    }

    /// Returns the provider-neutral model response.
    pub fn response(&self) -> &ModelResponse {
        &self.response
    }

    /// Consumes the completion and returns its provider-neutral response.
    pub fn into_response(self) -> ModelResponse {
        self.response
    }
}

/// Provider-reported facts about one completed model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionMetadata {
    finish_reason: String,
    response_id: Option<String>,
    response_model: Option<String>,
    system_fingerprint: Option<String>,
    usage: Option<CompletionUsage>,
}

impl CompletionMetadata {
    /// Creates normalized provider completion metadata.
    pub fn new(
        finish_reason: String,
        response_id: Option<String>,
        response_model: Option<String>,
        system_fingerprint: Option<String>,
        usage: Option<CompletionUsage>,
    ) -> Self {
        Self {
            finish_reason,
            response_id,
            response_model,
            system_fingerprint,
            usage,
        }
    }

    /// Returns the provider's reason that generation stopped.
    pub fn finish_reason(&self) -> &str {
        &self.finish_reason
    }

    /// Returns the provider-assigned response identifier, when reported.
    pub fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    /// Returns the model identifier reported in the response, when present.
    pub fn response_model(&self) -> Option<&str> {
        self.response_model.as_deref()
    }

    /// Returns the provider's backend fingerprint, when reported.
    pub fn system_fingerprint(&self) -> Option<&str> {
        self.system_fingerprint.as_deref()
    }

    /// Returns provider-reported token usage, when present.
    pub fn usage(&self) -> Option<&CompletionUsage> {
        self.usage.as_ref()
    }
}

/// Provider-reported token counts for one completed model request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionUsage {
    cache_hit: Option<u64>,
    cache_miss: Option<u64>,
    input: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
    total: Option<u64>,
}

impl CompletionUsage {
    /// Creates normalized provider-reported token usage.
    pub fn new(
        cache_hit_tokens: Option<u64>,
        cache_miss_tokens: Option<u64>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        reasoning_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        Self {
            cache_hit: cache_hit_tokens,
            cache_miss: cache_miss_tokens,
            input: input_tokens,
            output: output_tokens,
            reasoning: reasoning_tokens,
            total: total_tokens,
        }
    }

    /// Returns input tokens served from a provider cache, when reported.
    pub fn cache_hit_tokens(self) -> Option<u64> {
        self.cache_hit
    }

    /// Returns input tokens that missed a provider cache, when reported.
    pub fn cache_miss_tokens(self) -> Option<u64> {
        self.cache_miss
    }

    /// Returns the provider-reported input token count.
    pub fn input_tokens(self) -> Option<u64> {
        self.input
    }

    /// Returns the provider-reported output token count.
    pub fn output_tokens(self) -> Option<u64> {
        self.output
    }

    /// Returns output tokens used for provider-exposed reasoning, when
    /// reported.
    pub fn reasoning_tokens(self) -> Option<u64> {
        self.reasoning
    }

    /// Returns the provider-reported total token count.
    pub fn total_tokens(self) -> Option<u64> {
        self.total
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

    pub(crate) fn response_type(&self) -> ModelResponseType {
        match self {
            Self::Output(_) => ModelResponseType::Output,
            Self::ToolCall(_) => ModelResponseType::ToolCall,
        }
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
    /// A terminal response also contained native tool calls.
    #[error("model terminal response contained tool calls")]
    TerminalResponseWithToolCalls,
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

/// Stable, low-cardinality classification for a [`ModelError`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModelErrorType {
    /// Request construction or another unclassified client-side failure.
    Request,
    /// Network transport failed before a provider response was decoded.
    Transport,
    /// The provider returned an unsuccessful HTTP response.
    Provider,
    /// The provider returned a malformed response envelope.
    InvalidProviderResponse,
    /// The provider returned an unusable or incomplete successful response.
    InvalidResponse,
    /// The provider cannot satisfy the requested output contract.
    UnsupportedOutput,
    /// The response exceeded a configured safety bound.
    ResponseTooLarge,
    /// Terminal output failed JSON parsing or local schema validation.
    InvalidOutput,
    /// A native tool call was missing, malformed, or unsupported.
    InvalidToolCall,
}

impl ModelErrorType {
    /// Returns the stable value intended for telemetry attributes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request_error",
            Self::Transport => "transport_error",
            Self::Provider => "provider_error",
            Self::InvalidProviderResponse => "invalid_provider_response",
            Self::InvalidResponse => "invalid_response",
            Self::UnsupportedOutput => "unsupported_output",
            Self::ResponseTooLarge => "response_too_large",
            Self::InvalidOutput => "invalid_output",
            Self::InvalidToolCall => "invalid_tool_call",
        }
    }
}

#[derive(Debug, Error)]
#[error("{source}")]
struct ClassifiedRequestError {
    error_type: ModelErrorType,
    #[source]
    source: Box<dyn Error + Send + Sync>,
}

#[derive(Debug, Error)]
#[error("{provider} returned HTTP {status}: {body}")]
struct ProviderRequestError {
    body: String,
    provider: &'static str,
    #[source]
    source: reqwest::Error,
    status: reqwest::StatusCode,
}

impl ModelError {
    /// Wraps a provider transport or response-decoding failure.
    pub fn request(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Request(Box::new(error))
    }

    /// Returns a stable, low-cardinality classification for this failure.
    pub fn error_type(&self) -> ModelErrorType {
        match self {
            Self::Request(source) => {
                if source.downcast_ref::<ProviderRequestError>().is_some() {
                    ModelErrorType::Provider
                } else {
                    source
                        .downcast_ref::<ClassifiedRequestError>()
                        .map_or(ModelErrorType::Request, |error| error.error_type)
                }
            }
            Self::InvalidResponse | Self::IncompleteResponse { .. } => {
                ModelErrorType::InvalidResponse
            }
            Self::ResponseBodyTooLarge | Self::ResponseContentTooLarge => {
                ModelErrorType::ResponseTooLarge
            }
            Self::UnsupportedOutputSchema { .. } => ModelErrorType::UnsupportedOutput,
            Self::InvalidJson { .. } | Self::SchemaViolation { .. } => {
                ModelErrorType::InvalidOutput
            }
            Self::MissingToolCall
            | Self::MultipleToolCalls
            | Self::ToolCallWithContent
            | Self::TerminalResponseWithToolCalls
            | Self::UnsupportedToolType { .. }
            | Self::UnsupportedToolName { .. }
            | Self::InvalidToolArguments { .. } => ModelErrorType::InvalidToolCall,
        }
    }

    /// Returns the provider HTTP status associated with this failure, when
    /// available.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::Request(source) => source
                .downcast_ref::<ProviderRequestError>()
                .map(|error| error.status.as_u16()),
            _ => None,
        }
    }

    pub(crate) fn provider_request(
        provider: &'static str,
        body: String,
        source: reqwest::Error,
        status: reqwest::StatusCode,
    ) -> Self {
        Self::Request(Box::new(ProviderRequestError {
            body,
            provider,
            source,
            status,
        }))
    }

    pub(crate) fn classified_request(
        error_type: ModelErrorType,
        source: Box<dyn Error + Send + Sync>,
    ) -> Self {
        Self::Request(Box::new(ClassifiedRequestError { error_type, source }))
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
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::tool::{ReadArguments, ToolCall};

    struct ResponseOnlyModel;

    #[async_trait]
    impl Model for ResponseOnlyModel {
        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse::Output(json!({ "name": "Ada" })))
        }
    }

    struct MetadataModel;

    #[async_trait]
    impl ModelWithMetadata for MetadataModel {
        async fn complete_with_metadata(
            &self,
            _request: ModelRequest,
        ) -> Result<ModelCompletion, ModelError> {
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
    async fn response_only_model_defaults_optional_metadata_to_none() {
        // Arrange
        let model = ResponseOnlyModel;

        // Act
        let (response, metadata) = model
            .complete_with_optional_metadata(test_request())
            .await
            .expect("response-only model should complete");

        // Assert
        assert_eq!(response.output(), Some(&json!({ "name": "Ada" })));
        assert!(metadata.is_none());
    }

    #[tokio::test]
    async fn metadata_model_automatically_implements_model_paths() {
        // Arrange
        let model = MetadataModel;

        // Act
        let response = Model::complete(&model, test_request())
            .await
            .expect("metadata model should complete through Model");
        let (optional_response, metadata) =
            Model::complete_with_optional_metadata(&model, test_request())
                .await
                .expect("metadata model should expose optional metadata");

        // Assert
        assert_eq!(response.output(), Some(&json!({ "name": "Ada" })));
        assert_eq!(optional_response.output(), Some(&json!({ "name": "Ada" })));
        assert_eq!(
            metadata.as_ref().and_then(CompletionMetadata::response_id),
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

        // Assert
        assert_eq!(metadata.provider(), "alibaba_cloud");
        assert_eq!(metadata.model(), "qwen-plus");
        assert_eq!(
            metadata,
            &ModelMetadata::new("alibaba_cloud", "qwen-plus").expect("metadata should be valid")
        );
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
            crate::LifecycleEventKind::ModelRequestStarted { .. }
        ));
        assert!(matches!(
            events[1].kind(),
            crate::LifecycleEventKind::ModelRequestCompleted { .. }
        ));
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
            crate::LifecycleEventKind::ModelRequestFailed {
                error_type: ModelErrorType::Provider,
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
    fn request_deduplicates_native_tools() {
        // Arrange
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

        // Act
        let request = ModelRequest::new("hello", schema)
            .with_tool(tool::ToolDefinition::read())
            .with_tool(tool::ToolDefinition::read());

        // Assert
        assert_eq!(request.tools(), &[tool::ToolDefinition::read()]);
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
        let completion = ModelCompletion::new(metadata, response.clone());

        // Act
        let completion_metadata = completion.metadata();
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
                ModelError::ToolCallWithContent,
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
    fn classified_request_retains_source_and_type() {
        // Arrange
        let error = ModelError::classified_request(
            ModelErrorType::Transport,
            io::Error::other("connection reset").into(),
        );

        // Act
        let source = std::error::Error::source(&error)
            .and_then(std::error::Error::source)
            .expect("classified request should retain its original source");

        // Assert
        assert_eq!(error.error_type(), ModelErrorType::Transport);
        assert_eq!(error.http_status(), None);
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
}
