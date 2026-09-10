use std::collections::HashSet;
use std::error::Error;
use std::ops::Deref;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::lifecycle::{LifecycleEmitter, LifecycleObserver, ModelResponseType};
use crate::provider::{self, KimiConfig, MuseConfig, QwenConfig};
use crate::schema_contract::{OutputSchema, OutputValidationError, bounded_diagnostic};
use crate::{chat_completion, telemetry, tool};

/// Object-safe boundary for provider-neutral model requests.
///
/// [`ModelClient`] implements this trait so applications can select supported
/// providers dynamically without exposing provider backends or raw generation.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Model: Send + Sync {
    /// Returns the configured model identity when the implementation exposes
    /// it.
    fn metadata(&self) -> Option<ModelMetadata> {
        None
    }

    /// Completes one model request with optional provider metadata and
    /// continuation state.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError>;
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
        let policy = provider::kimi_policy(&config.model);

        Self::chat_completion(config.api_key, config.base_url, config.model, policy)
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
        let policy = provider::qwen_policy(&config.model);

        Self::chat_completion(config.api_key, config.base_url, config.model, policy)
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
    pub async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let metrics = telemetry::RequestMetrics::start(self.metadata());
        let lifecycle = if request.lifecycle_observed() {
            None
        } else {
            self.lifecycle
                .start_model_request(Some(self.metadata.clone()), 0, None)
        };
        let operation = self.backend.generate(&request);
        let generated = match lifecycle.as_ref() {
            Some(lifecycle) => lifecycle.scope(operation).await,
            None => operation.await,
        };
        let (result, failure_metadata) = match generated {
            Ok(chat_completion::GeneratedResponse::Failed { error, metadata }) => {
                (Err(error), Some(metadata))
            }
            Ok(chat_completion::GeneratedResponse::Output {
                metadata,
                output,
                reasoning_content,
            }) => match request.schema().parse_and_validate(&output) {
                Ok(response) => (
                    Ok(
                        ModelCompletion::new(metadata, ModelResponse::from_output(response))
                            .with_reasoning_content(reasoning_content),
                    ),
                    None,
                ),
                Err(error) => (Err(ModelError::from(error)), Some(metadata)),
            },
            Ok(chat_completion::GeneratedResponse::ToolCall { call, metadata }) => (
                Ok(ModelCompletion::new(
                    metadata,
                    ModelResponse::tool_call(call),
                )),
                None,
            ),
            Ok(chat_completion::GeneratedResponse::ToolCalls { calls, metadata }) => (
                Ok(ModelCompletion::new(
                    metadata,
                    ModelResponse::tool_calls(calls),
                )),
                None,
            ),
            Err(error) => (Err(error), None),
        };

        match &result {
            Ok(completion) => {
                if let Some(metadata) = completion.metadata() {
                    metrics.completed(metadata);
                }
            }
            Err(error) => metrics.failed(error, failure_metadata.as_ref()),
        }

        if let Some(lifecycle) = lifecycle {
            match &result {
                Ok(completion) => lifecycle.completed(
                    completion.metadata.clone(),
                    completion.response.response_type(),
                ),
                Err(error) => lifecycle.failed(error.error_type(), error.http_status()),
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
impl Model for ModelClient {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(self.metadata.clone())
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
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
    lifecycle_observed: bool,
    messages: Vec<ModelMessage>,
    model_reasoning_effort: Option<ReasoningEffort>,
    prompt: String,
    provider_session_id: Option<String>,
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
            model_reasoning_effort: None,
            prompt,
            provider_session_id: None,
            schema,
            tools: Vec::new(),
        }
    }

    pub(crate) fn with_history(
        messages: Vec<ModelMessage>,
        prompt: impl Into<String>,
        schema: OutputSchema,
    ) -> Self {
        let prompt = prompt.into();
        let mut messages = messages;
        messages.push(ModelMessage::User(prompt.clone()));

        Self {
            lifecycle_observed: false,
            messages,
            model_reasoning_effort: None,
            prompt,
            provider_session_id: None,
            schema,
            tools: Vec::new(),
        }
    }

    /// Requests a provider-supported reasoning depth for this completion.
    #[must_use]
    pub fn with_model_reasoning_effort(mut self, reasoning_effort: ReasoningEffort) -> Self {
        self.model_reasoning_effort = Some(reasoning_effort);

        self
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

    /// Returns the opaque provider conversation identifier to resume, when
    /// native continuation is available.
    pub fn provider_session_id(&self) -> Option<&str> {
        self.provider_session_id.as_deref()
    }

    /// Returns the schema that the response must match.
    pub fn schema(&self) -> &OutputSchema {
        &self.schema
    }

    /// Returns the native function tools explicitly advertised by the caller.
    pub fn tools(&self) -> &[tool::ToolDefinition] {
        &self.tools
    }

    /// Returns the ordered conversation to send to the provider, including
    /// the system prompt, retained turns, current prompt, and tool results.
    ///
    /// Model adapters must use this history rather than only [`Self::prompt`]
    /// to support chat and tool execution. The harness owns its mutation.
    pub fn messages(&self) -> &[ModelMessage] {
        &self.messages
    }

    /// Returns the requested provider reasoning depth, when configured.
    pub fn model_reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.model_reasoning_effort
    }

    pub(crate) fn advertises_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name() == name)
    }

    pub(crate) fn lifecycle_observed(&self) -> bool {
        self.lifecycle_observed
    }

    pub(crate) fn mark_lifecycle_observed(&mut self) {
        self.lifecycle_observed = true;
    }

    pub(crate) fn set_provider_session_id(&mut self, provider_session_id: Option<String>) {
        self.provider_session_id = provider_session_id;
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

    pub(crate) fn record_tool_results(
        &mut self,
        calls: Vec<tool::ToolCall>,
        contents: Vec<String>,
    ) {
        debug_assert_eq!(calls.len(), contents.len());
        let results: Vec<_> = calls
            .iter()
            .zip(contents)
            .map(|(call, content)| (call.id().to_string(), call.name().to_string(), content))
            .collect();
        self.messages.push(ModelMessage::AssistantToolCalls(calls));
        self.messages
            .extend(
                results
                    .into_iter()
                    .map(|(call_id, name, content)| ModelMessage::ToolResult {
                        call_id,
                        content,
                        name,
                    }),
            );
    }

    pub(crate) fn record_output_with_reasoning(
        &mut self,
        output: &Value,
        reasoning_content: Option<String>,
    ) {
        let content = output.to_string();
        self.messages.push(match reasoning_content {
            Some(reasoning_content) => ModelMessage::AssistantReasoning {
                content,
                reasoning_content,
            },
            None => ModelMessage::Assistant(content),
        });
    }

    pub(crate) fn into_messages(self) -> Vec<ModelMessage> {
        self.messages
    }
}

/// Provider-supported reasoning depth for one model completion.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasoningEffort {
    /// Uses a light reasoning pass.
    Low,
    /// Uses a moderate reasoning pass.
    Medium,
    /// Uses a deep reasoning pass.
    #[default]
    High,
    /// Uses an extra-high reasoning pass.
    #[serde(rename = "xhigh")]
    XHigh,
    /// Uses the provider's maximum reasoning depth.
    Max,
}

impl ReasoningEffort {
    /// All selectable model reasoning efforts in display order.
    pub const ALL: [Self; 5] = [Self::Low, Self::Medium, Self::High, Self::XHigh, Self::Max];

    /// Returns the provider-neutral identifier for this effort.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Provider-neutral conversation entry supplied through
/// [`ModelRequest::messages`].
///
/// Tool results retain their call identifiers so adapters can correlate them
/// with the preceding assistant calls without parsing provider wire formats.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModelMessage {
    /// Validated structured assistant output serialized as JSON text.
    Assistant(String),
    /// Validated assistant output paired with provider reasoning that must be
    /// replayed.
    AssistantReasoning {
        /// Validated structured assistant output serialized as JSON text.
        content: String,
        /// Provider reasoning content replayed verbatim on the next request.
        reasoning_content: String,
    },
    /// One assistant tool call, followed by its result.
    AssistantToolCall(tool::ToolCall),
    /// An ordered assistant tool-call batch, followed by its ordered results.
    AssistantToolCalls(Vec<tool::ToolCall>),
    /// Application-provided instructions for the conversation.
    System(String),
    /// Harness-produced feedback for one assistant tool call.
    ToolResult {
        /// Identifier of the corresponding assistant tool call.
        call_id: String,
        /// Serialized tool output or corrective failure feedback.
        content: String,
        /// Built-in tool name.
        name: String,
    },
    /// User prompt for a retained or current turn.
    User(String),
}

impl ModelMessage {
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Assistant(content) | Self::System(content) | Self::User(content) => content.len(),
            Self::AssistantReasoning {
                content,
                reasoning_content,
            } => content.len().saturating_add(reasoning_content.len()),
            Self::AssistantToolCall(call) => {
                let arguments = call
                    .arguments_json()
                    .map_or(usize::MAX, |arguments| arguments.len());

                call.id()
                    .len()
                    .saturating_add(call.name().len())
                    .saturating_add(arguments)
                    .saturating_add(call.reasoning_content().map_or(0, str::len))
            }
            Self::AssistantToolCalls(calls) => calls.iter().fold(0, |bytes, call| {
                let arguments = call
                    .arguments_json()
                    .map_or(usize::MAX, |arguments| arguments.len());

                bytes
                    .saturating_add(call.id().len())
                    .saturating_add(call.name().len())
                    .saturating_add(arguments)
                    .saturating_add(call.reasoning_content().map_or(0, str::len))
            }),
            Self::ToolResult {
                call_id,
                content,
                name,
            } => call_id
                .len()
                .saturating_add(content.len())
                .saturating_add(name.len()),
        }
    }
}

/// One model response paired with normalized provider completion metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCompletion {
    metadata: Option<CompletionMetadata>,
    provider_session_id: Option<String>,
    reasoning_content: Option<String>,
    response: ModelResponse,
}

impl ModelCompletion {
    /// Creates a completion from normalized metadata and a model response.
    pub fn new(metadata: CompletionMetadata, response: ModelResponse) -> Self {
        Self {
            metadata: Some(metadata),
            provider_session_id: None,
            reasoning_content: None,
            response,
        }
    }

    /// Creates a completion without provider-reported metadata.
    pub fn from_response(response: ModelResponse) -> Self {
        Self {
            metadata: None,
            provider_session_id: None,
            reasoning_content: None,
            response,
        }
    }

    /// Attaches the opaque provider session identifier returned by this turn.
    #[must_use]
    pub fn with_provider_session_id(mut self, provider_session_id: impl Into<String>) -> Self {
        self.provider_session_id = Some(provider_session_id.into());

        self
    }

    pub(crate) fn with_reasoning_content(mut self, reasoning_content: Option<String>) -> Self {
        self.reasoning_content = reasoning_content;

        self
    }

    /// Returns the normalized metadata reported by the provider.
    pub fn metadata(&self) -> Option<&CompletionMetadata> {
        self.metadata.as_ref()
    }

    /// Returns the opaque provider session identifier for the next turn.
    pub fn provider_session_id(&self) -> Option<&str> {
        self.provider_session_id.as_deref()
    }

    /// Returns the provider-neutral model response.
    pub fn response(&self) -> &ModelResponse {
        &self.response
    }

    /// Consumes the completion and returns its provider-neutral response.
    pub fn into_response(self) -> ModelResponse {
        self.response
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ModelResponse,
        Option<CompletionMetadata>,
        Option<String>,
        Option<String>,
    ) {
        (
            self.response,
            self.metadata,
            self.provider_session_id,
            self.reasoning_content,
        )
    }
}

impl Deref for ModelCompletion {
    type Target = ModelResponse;

    fn deref(&self) -> &Self::Target {
        &self.response
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
    /// Terminal structured model output.
    ///
    /// [`ModelClient`] and [`crate::Harness`] validate this value locally
    /// against the request schema before returning it to applications.
    Output(Value),
    /// One validated native function call requiring application handling.
    ToolCall(tool::ToolCall),
    /// Multiple validated native function calls from one model response.
    ///
    /// The harness rejects an empty batch as a missing tool call.
    ToolCalls(Vec<tool::ToolCall>),
}

impl ModelResponse {
    /// Returns terminal structured output, when present.
    pub fn output(&self) -> Option<&Value> {
        match self {
            Self::Output(output) => Some(output),
            Self::ToolCall(_) | Self::ToolCalls(_) => None,
        }
    }

    /// Returns the intermediate native function call, when present.
    pub fn call(&self) -> Option<&tool::ToolCall> {
        match self {
            Self::ToolCall(call) => Some(call),
            Self::Output(_) | Self::ToolCalls(_) => None,
        }
    }

    /// Returns every intermediate native function call in this response.
    pub fn calls(&self) -> &[tool::ToolCall] {
        match self {
            Self::Output(_) => &[],
            Self::ToolCall(call) => std::slice::from_ref(call),
            Self::ToolCalls(calls) => calls,
        }
    }

    fn from_output(output: Value) -> Self {
        Self::Output(output)
    }

    fn tool_call(call: tool::ToolCall) -> Self {
        Self::ToolCall(call)
    }

    fn tool_calls(calls: Vec<tool::ToolCall>) -> Self {
        Self::ToolCalls(calls)
    }

    pub(crate) fn response_type(&self) -> ModelResponseType {
        match self {
            Self::Output(_) => ModelResponseType::Output,
            Self::ToolCall(_) | Self::ToolCalls(_) => ModelResponseType::ToolCall,
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
    /// The provider could not restore the requested native session.
    #[error("provider session is unavailable")]
    ResumeUnavailable,
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
    /// The provider tool-call identifier is blank or exceeds its byte limit.
    #[error("model returned a blank or oversized tool call identifier")]
    InvalidToolCallId,
    /// The provider returned more than the single supported call.
    #[error("model returned multiple tool calls")]
    MultipleToolCalls,
    /// The provider returned multiple tool calls with the same identifier.
    #[error("model returned duplicate tool call identifier: {id}")]
    DuplicateToolCallId {
        /// Bounded duplicate identifier returned by the provider.
        id: String,
    },
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
            Self::ResumeUnavailable => ModelErrorType::Provider,
            Self::ResponseBodyTooLarge | Self::ResponseContentTooLarge => {
                ModelErrorType::ResponseTooLarge
            }
            Self::UnsupportedOutputSchema { .. } => ModelErrorType::UnsupportedOutput,
            Self::InvalidJson { .. } | Self::SchemaViolation { .. } => {
                ModelErrorType::InvalidOutput
            }
            Self::MissingToolCall
            | Self::InvalidToolCallId
            | Self::MultipleToolCalls
            | Self::DuplicateToolCallId { .. }
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
                .map(|error| error.status.as_u16())
                .or_else(|| {
                    source
                        .downcast_ref::<ClassifiedRequestError>()
                        .and_then(|error| error.http_status)
                }),
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
        http_status: Option<u16>,
        source: Box<dyn Error + Send + Sync>,
    ) -> Self {
        Self::Request(Box::new(ClassifiedRequestError {
            error_type,
            http_status,
            source,
        }))
    }
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
            Self::Request => telemetry::ERROR_REQUEST,
            Self::Transport => telemetry::ERROR_TRANSPORT,
            Self::Provider => telemetry::ERROR_PROVIDER,
            Self::InvalidProviderResponse => telemetry::ERROR_INVALID_PROVIDER_RESPONSE,
            Self::InvalidResponse => telemetry::ERROR_INVALID_RESPONSE,
            Self::UnsupportedOutput => telemetry::ERROR_UNSUPPORTED_OUTPUT,
            Self::ResponseTooLarge => telemetry::ERROR_RESPONSE_TOO_LARGE,
            Self::InvalidOutput => telemetry::ERROR_INVALID_OUTPUT,
            Self::InvalidToolCall => telemetry::ERROR_INVALID_TOOL_CALL,
        }
    }
}

#[derive(Debug, Error)]
#[error("{source}")]
struct ClassifiedRequestError {
    error_type: ModelErrorType,
    http_status: Option<u16>,
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

pub(crate) fn ensure_unique_tool_call_ids(calls: &[tool::ToolCall]) -> Result<(), ModelError> {
    let mut call_ids = HashSet::with_capacity(calls.len());
    for call in calls {
        if !call_ids.insert(call.id()) {
            return Err(ModelError::DuplicateToolCallId {
                id: bounded_diagnostic(call.id()),
            });
        }
    }

    Ok(())
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
#[path = "model_test.rs"]
mod tests;
