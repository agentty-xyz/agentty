//! The model boundary and model selection.
//!
//! Implement [`Model`] to plug in any provider, or use the built-in clients in
//! [`crate::provider`]. [`ModelRegistry`] selects models by stable host keys
//! with declared [`ModelCapabilities`], whose required [`ContextBudget`]
//! bounds every request through model-aware history projection.

use std::collections::HashSet;
use std::error::Error;
use std::ops::Deref;

pub use ag_router::{CompletionMetadata, CompletionUsage};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

pub use crate::context::{
    ContextBudget, ContextBudgetError, ContextEstimator, HeuristicContextEstimator,
};
use crate::input::TurnInput;
use crate::lifecycle::{LifecycleEmitter, LifecycleObserver, ModelResponseType};
pub use crate::model_registry::{
    ModelCapabilities, ModelRegistration, ModelRegistry, ModelRegistryError,
};
use crate::provider::{KimiConfig, MuseConfig, QwenConfig};
use crate::schema_contract::{OutputSchema, OutputValidationError, bounded_diagnostic};
use crate::{telemetry, tool};

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

    /// Checks adapter-specific output-schema requirements without execution.
    /// Implementations accepting every validated schema may use the default.
    ///
    /// # Errors
    /// Returns an unsupported-schema error before committing a model switch.
    fn validate_schema(&self, schema: &OutputSchema) -> Result<(), ModelError> {
        let _ = schema;

        Ok(())
    }

    /// Checks adapter-specific turn-input requirements without execution.
    /// Image content is opt-in: the default rejects image-bearing input, so
    /// implementations that read images must override this method.
    ///
    /// # Errors
    /// Returns [`ModelError::UnsupportedImageInput`] before turn acquisition
    /// when the adapter cannot accept the input.
    fn validate_input(&self, input: &TurnInput) -> Result<(), ModelError> {
        if input.has_images() {
            return Err(ModelError::UnsupportedImageInput {
                reason: "model does not declare image input support".to_string(),
            });
        }

        Ok(())
    }

    /// Completes one model request with optional provider metadata.
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
/// [`ModelClient::complete`], which owns telemetry while `ag-router` validates
/// structured output.
pub struct ModelClient {
    router: ag_router::Router,
    router_model: String,
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
        Self::routed(
            ag_router::Provider::Kimi,
            config.api_key,
            config.base_url,
            &config.model,
        )
    }

    /// Creates a client backed by Meta's Model API for Muse models.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    pub fn muse(config: MuseConfig) -> Result<Self, ModelMetadataError> {
        Self::routed(
            ag_router::Provider::Muse,
            config.api_key,
            config.base_url,
            &config.model,
        )
    }

    /// Creates a client backed by Alibaba Cloud Model Studio's Qwen API.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    pub fn qwen(config: QwenConfig) -> Result<Self, ModelMetadataError> {
        Self::routed(
            ag_router::Provider::Qwen,
            config.api_key,
            config.base_url,
            &config.model,
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
    pub async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let metrics = telemetry::RequestMetrics::start(self.metadata());
        let lifecycle = if request.lifecycle_observed() {
            None
        } else {
            self.lifecycle
                .start_model_request(Some(self.metadata.clone()), 0, None)
        };
        let operation = async {
            let routed = self
                .to_router_request(&request)
                .map_err(|error| (error, None))?;
            self.router.execute(routed).await.map_err(|error| {
                let metadata = error.metadata().cloned();
                (Self::from_router_error(error), metadata)
            })
        };
        let generated = match lifecycle.as_ref() {
            Some(lifecycle) => lifecycle.scope(operation).await,
            None => operation.await,
        };
        let (result, failure_metadata) = match generated {
            Ok(completion) => {
                let metadata = match &completion {
                    ag_router::Completion::Output { metadata, .. }
                    | ag_router::Completion::ToolCalls { metadata, .. } => metadata.clone(),
                };
                let result = Self::from_router_completion(completion);
                let failure_metadata = result.as_ref().err().map(|_| metadata);
                (result, failure_metadata)
            }
            Err((error, metadata)) => (Err(error), metadata),
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

    fn routed(
        provider: ag_router::Provider,
        api_key: String,
        base_url: String,
        model: &str,
    ) -> Result<Self, ModelMetadataError> {
        let metadata = ModelMetadata::new(provider_telemetry_name(provider), model)?;
        let router_model = format!("{}/{model}", provider.as_str());
        let router = ag_router::Router::single(ag_router::ProviderConfig {
            provider,
            api_key,
            base_url,
        });

        Ok(Self {
            router,
            router_model,
            lifecycle: LifecycleEmitter::default(),
            metadata,
        })
    }

    fn to_router_request(
        &self,
        request: &ModelRequest,
    ) -> Result<ag_router::ModelRequest, ModelError> {
        let format =
            ag_router::JsonSchemaFormat::new("ag_harness_output", request.schema().clone())
                .map_err(Self::from_router_error)?;
        let messages = request
            .messages()
            .iter()
            .map(Self::to_router_message)
            .collect::<Result<Vec<_>, _>>()?;
        let tools = request
            .tools()
            .iter()
            .map(|tool| {
                ag_router::ToolDefinition::new(
                    tool.name(),
                    tool.description(),
                    tool.parameters().clone(),
                )
            })
            .collect();
        let mut routed = ag_router::ModelRequest::chat(&self.router_model, messages, tools, format);
        routed.options.reasoning_effort =
            request.model_reasoning_effort().map(|effort| match effort {
                ReasoningEffort::Low => ag_router::ReasoningEffort::Low,
                ReasoningEffort::Medium => ag_router::ReasoningEffort::Medium,
                ReasoningEffort::High => ag_router::ReasoningEffort::High,
                ReasoningEffort::XHigh => ag_router::ReasoningEffort::XHigh,
                ReasoningEffort::Max => ag_router::ReasoningEffort::Max,
            });
        Ok(routed)
    }

    fn to_router_message(message: &ModelMessage) -> Result<ag_router::ModelMessage, ModelError> {
        use ag_router::ModelMessage as Routed;
        Ok(match message {
            ModelMessage::Assistant(content) => Routed::Assistant(content.clone()),
            ModelMessage::AssistantReasoning {
                content,
                reasoning_content,
            } => Routed::AssistantReasoning {
                content: content.clone(),
                reasoning_content: reasoning_content.clone(),
            },
            ModelMessage::AssistantToolCall(call) => {
                Routed::AssistantToolCall(Self::to_router_call(call)?)
            }
            ModelMessage::AssistantToolCalls(calls) => Routed::AssistantToolCalls(
                calls
                    .iter()
                    .map(Self::to_router_call)
                    .collect::<Result<_, _>>()?,
            ),
            ModelMessage::System(content) => Routed::System(content.clone()),
            ModelMessage::ToolResult {
                call_id,
                content,
                name,
            } => Routed::ToolResult {
                call_id: call_id.clone(),
                content: content.clone(),
                name: name.clone(),
            },
            ModelMessage::User(content) => Routed::User(content.clone()),
            ModelMessage::UserInput(input) => Routed::UserInput(Self::to_router_input(input)?),
        })
    }

    fn to_router_input(input: &TurnInput) -> Result<ag_router::TurnInput, ModelError> {
        let blocks = input
            .blocks()
            .iter()
            .map(|block| {
                Ok(match block {
                    crate::InputBlock::Text(text) => ag_router::InputBlock::Text(text.clone()),
                    crate::InputBlock::Image(image) => ag_router::InputBlock::Image(
                        ag_router::ImageContent::from_shared(
                            match image.media_type() {
                                crate::ImageMediaType::Jpeg => ag_router::ImageMediaType::Jpeg,
                                crate::ImageMediaType::Png => ag_router::ImageMediaType::Png,
                            },
                            image.shared_bytes(),
                        )
                        .map_err(ModelError::request)?,
                    ),
                })
            })
            .collect::<Result<Vec<_>, ModelError>>()?;

        ag_router::TurnInput::new(blocks).map_err(ModelError::request)
    }

    fn to_router_call(call: &tool::ToolCall) -> Result<ag_router::ToolCall, ModelError> {
        ag_router::ToolCall::from_json(
            call.id().to_string(),
            call.name(),
            &call.arguments_json().map_err(ModelError::request)?,
            call.reasoning_content().map(str::to_string),
        )
        .map_err(Self::from_router_error)
    }

    fn from_router_completion(
        completion: ag_router::Completion,
    ) -> Result<ModelCompletion, ModelError> {
        Ok(match completion {
            ag_router::Completion::Output {
                value,
                metadata,
                reasoning_content,
            } => ModelCompletion::new(metadata, ModelResponse::Output(value))
                .with_reasoning_content(reasoning_content),
            ag_router::Completion::ToolCalls { calls, metadata } => {
                let mut calls = calls
                    .into_iter()
                    .map(|call| {
                        tool::ToolCall::from_json(
                            call.id().to_string(),
                            call.name(),
                            &call.arguments_json(),
                            call.reasoning_content().map(str::to_string),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let response = if calls.len() == 1 {
                    ModelResponse::ToolCall(calls.remove(0))
                } else {
                    ModelResponse::ToolCalls(calls)
                };
                ModelCompletion::new(metadata, response)
            }
        })
    }

    fn from_router_error(error: ag_router::ModelError) -> ModelError {
        let error_type = match error.error_class() {
            ag_router::ErrorClass::Request => ModelErrorType::Request,
            ag_router::ErrorClass::Transport => ModelErrorType::Transport,
            ag_router::ErrorClass::Provider => ModelErrorType::Provider,
            ag_router::ErrorClass::InvalidProviderResponse => {
                ModelErrorType::InvalidProviderResponse
            }
            ag_router::ErrorClass::InvalidResponse => ModelErrorType::InvalidResponse,
            ag_router::ErrorClass::UnsupportedOutput => ModelErrorType::UnsupportedOutput,
            ag_router::ErrorClass::UnsupportedInput => ModelErrorType::UnsupportedInput,
            ag_router::ErrorClass::ResponseTooLarge => ModelErrorType::ResponseTooLarge,
            ag_router::ErrorClass::InvalidOutput => ModelErrorType::InvalidOutput,
            ag_router::ErrorClass::InvalidToolCall => ModelErrorType::InvalidToolCall,
        };
        let http_status = error.http_status();
        match error {
            ag_router::ModelError::WithMetadata { source, .. } => Self::from_router_error(*source),
            ag_router::ModelError::InvalidResponse => ModelError::InvalidResponse,
            ag_router::ModelError::IncompleteResponse { reason } => {
                ModelError::IncompleteResponse { reason }
            }
            ag_router::ModelError::ResponseBodyTooLarge => ModelError::ResponseBodyTooLarge,
            ag_router::ModelError::UnsupportedOutputSchema { reason } => {
                ModelError::UnsupportedOutputSchema { reason }
            }
            ag_router::ModelError::UnsupportedImageInput { reason } => {
                ModelError::UnsupportedImageInput { reason }
            }
            ag_router::ModelError::ResponseContentTooLarge => ModelError::ResponseContentTooLarge,
            ag_router::ModelError::InvalidJson { reason } => ModelError::InvalidJson { reason },
            ag_router::ModelError::SchemaViolation { path, reason } => {
                ModelError::SchemaViolation { path, reason }
            }
            ag_router::ModelError::MissingToolCall => ModelError::MissingToolCall,
            ag_router::ModelError::InvalidToolCallId => ModelError::InvalidToolCallId,
            ag_router::ModelError::DuplicateToolCallId { id } => {
                ModelError::DuplicateToolCallId { id }
            }
            ag_router::ModelError::TerminalResponseWithToolCalls => {
                ModelError::TerminalResponseWithToolCalls
            }
            ag_router::ModelError::UnsupportedToolType { kind } => {
                ModelError::UnsupportedToolType { kind }
            }
            ag_router::ModelError::UnsupportedToolName { name } => {
                ModelError::UnsupportedToolName { name }
            }
            ag_router::ModelError::InvalidToolArguments { reason } => {
                ModelError::InvalidToolArguments { reason }
            }
            error => ModelError::classified_request(error_type, http_status, error.into_source()),
        }
    }
}

fn provider_telemetry_name(provider: ag_router::Provider) -> &'static str {
    match provider {
        ag_router::Provider::Kimi => telemetry::PROVIDER_MOONSHOT_AI,
        ag_router::Provider::Muse => telemetry::PROVIDER_META,
        ag_router::Provider::Qwen => telemetry::PROVIDER_ALIBABA_CLOUD,
    }
}

#[async_trait]
impl Model for ModelClient {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(self.metadata.clone())
    }

    fn validate_schema(&self, schema: &OutputSchema) -> Result<(), ModelError> {
        self.router
            .validate_schema(&self.router_model, schema)
            .map_err(Self::from_router_error)
    }

    fn validate_input(&self, input: &TurnInput) -> Result<(), ModelError> {
        let router_input = Self::to_router_input(input)?;
        self.router
            .validate_input(&self.router_model, &router_input)
            .map_err(Self::from_router_error)
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
    schema: OutputSchema,
    tools: Vec<tool::ToolDefinition>,
}

impl ModelRequest {
    /// Creates a model request whose response must match `schema`.
    ///
    /// Plain strings remain the text-only construction path; ordered
    /// text/image content uses an explicit [`TurnInput`].
    pub fn new(input: impl Into<TurnInput>, schema: OutputSchema) -> Self {
        Self::with_history(Vec::new(), input, schema)
    }

    pub(crate) fn with_history(
        messages: Vec<ModelMessage>,
        input: impl Into<TurnInput>,
        schema: OutputSchema,
    ) -> Self {
        let input = input.into();
        let prompt = input.joined_text();
        let mut messages = messages;
        messages.push(input.into_user_message());

        Self {
            lifecycle_observed: false,
            messages,
            model_reasoning_effort: None,
            prompt,
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

    /// Returns the current input's text content, excluding image blocks.
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

    pub(crate) fn record_tool_call(&mut self, call: tool::ToolCall) -> &ModelMessage {
        self.record(ModelMessage::AssistantToolCall(call))
    }

    pub(crate) fn record_tool_calls(&mut self, calls: Vec<tool::ToolCall>) -> &ModelMessage {
        self.record(ModelMessage::AssistantToolCalls(calls))
    }

    pub(crate) fn record_tool_result(
        &mut self,
        call: &tool::ToolCall,
        content: String,
    ) -> &ModelMessage {
        self.record(ModelMessage::ToolResult {
            call_id: call.id().to_string(),
            content,
            name: call.name().to_string(),
        })
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

    fn record(&mut self, message: ModelMessage) -> &ModelMessage {
        self.messages.push(message);

        &self.messages[self.messages.len() - 1]
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
    /// Ordered image-bearing user input for a retained or current turn.
    ///
    /// Text-only input is normalized to [`Self::User`]; this variant always
    /// carries at least one image block.
    UserInput(TurnInput),
}

impl ModelMessage {
    /// Payload bytes used by bounded session history, independent of storage
    /// encoding. Images count their base64 data-URL length.
    pub fn retained_bytes(&self) -> usize {
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
            Self::UserInput(input) => input.retained_bytes(),
        }
    }
}

/// One model response paired with normalized provider completion metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCompletion {
    metadata: Option<CompletionMetadata>,
    reasoning_content: Option<String>,
    response: ModelResponse,
}

impl ModelCompletion {
    /// Creates a completion from normalized metadata and a model response.
    pub fn new(metadata: CompletionMetadata, response: ModelResponse) -> Self {
        Self {
            metadata: Some(metadata),
            reasoning_content: None,
            response,
        }
    }

    /// Creates a completion without provider-reported metadata.
    pub fn from_response(response: ModelResponse) -> Self {
        Self {
            metadata: None,
            reasoning_content: None,
            response,
        }
    }

    pub(crate) fn with_reasoning_content(mut self, reasoning_content: Option<String>) -> Self {
        self.reasoning_content = reasoning_content;

        self
    }

    /// Returns the normalized metadata reported by the provider.
    pub fn metadata(&self) -> Option<&CompletionMetadata> {
        self.metadata.as_ref()
    }

    /// Returns the provider-neutral model response.
    pub fn response(&self) -> &ModelResponse {
        &self.response
    }

    /// Consumes the completion and returns its provider-neutral response.
    pub fn into_response(self) -> ModelResponse {
        self.response
    }

    pub(crate) fn into_parts(self) -> (ModelResponse, Option<CompletionMetadata>, Option<String>) {
        (self.response, self.metadata, self.reasoning_content)
    }
}

impl Deref for ModelCompletion {
    type Target = ModelResponse;

    fn deref(&self) -> &Self::Target {
        &self.response
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
    /// The model configuration cannot accept image input content.
    #[error("model does not support image input: {reason}")]
    UnsupportedImageInput {
        /// Provider-specific reason image content cannot be translated.
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
            Self::Request(source) => source
                .downcast_ref::<ClassifiedRequestError>()
                .map_or(ModelErrorType::Request, |error| error.error_type),
            Self::InvalidResponse | Self::IncompleteResponse { .. } => {
                ModelErrorType::InvalidResponse
            }
            Self::ResponseBodyTooLarge | Self::ResponseContentTooLarge => {
                ModelErrorType::ResponseTooLarge
            }
            Self::UnsupportedOutputSchema { .. } => ModelErrorType::UnsupportedOutput,
            Self::UnsupportedImageInput { .. } => ModelErrorType::UnsupportedInput,
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
                .downcast_ref::<ClassifiedRequestError>()
                .and_then(|error| error.http_status),
            _ => None,
        }
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
    /// The provider cannot accept the requested input content.
    UnsupportedInput,
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
            Self::UnsupportedInput => telemetry::ERROR_UNSUPPORTED_INPUT,
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
