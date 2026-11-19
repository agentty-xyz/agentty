//! The model boundary and model selection.
//!
//! Implement [`Model`] to plug in any provider, or use the built-in clients in
//! [`crate::provider`]. [`ModelRegistry`] selects models by stable host keys
//! with declared [`ModelCapabilities`], whose required [`ModelProfile`]
//! derives the [`ContextBudget`] bounding every request through model-aware
//! history projection.

use std::borrow::Cow;
use std::collections::HashSet;
use std::error::Error;
use std::ops::Deref;
use std::time::Duration;

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
use ag_router::ModelMessage as RoutedMessage;
pub use ag_router::{CompletionMetadata, CompletionUsage};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

pub use crate::context::{
    ContextBudget, ContextBudgetError, ContextEstimator, ContextPolicy, ContextSummaryPolicy,
    HeuristicContextEstimator,
};
use crate::input::TurnInput;
use crate::lifecycle::ModelResponseType;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
use crate::lifecycle::{LifecycleEmitter, LifecycleObserver};
pub use crate::model_profile::ModelProfile;
pub use crate::model_registry::{
    ModelCapabilities, ModelRegistration, ModelRegistry, ModelRegistryError,
};
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
use crate::provider::{self, ProviderSpec, ProviderSpecError};
#[cfg(feature = "providers-openai")]
use crate::provider::{ChatGptConfig, OpenAiConfig};
#[cfg(feature = "providers-compat")]
use crate::provider::{KimiConfig, MuseConfig, OpenAiCompatibleConfig, QwenConfig};
use crate::request_option::{GenerationParams, OutputMode};
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

    /// Returns the context profile the implementation declares for its
    /// model, or `None`, as by default, when the host declares it.
    ///
    /// [`crate::Harness::builder`] derives the request budget and estimator
    /// from this profile unless the host sets an explicit budget.
    /// [`ModelClient`] declares the cataloged profile of `OpenAI`, `ChatGPT`,
    /// Kimi, Muse, and Qwen flash models; OpenAI-compatible and uncataloged
    /// models declare none, so hosts set their budget explicitly.
    fn profile(&self) -> Option<ModelProfile> {
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

    /// Returns whether a request carrying [`ModelRequest::prefix_schema`]
    /// keeps the provider's cached prefix of the conversation it continues:
    /// the adapter states that schema where the conversation's requests
    /// stated theirs and leaves the request's own schema to its last
    /// message. The harness then asks for round summaries with the turn's
    /// last request and an appended instruction; otherwise, as by default,
    /// with a compact request of their own.
    fn keeps_prefix_across_schemas(&self) -> bool {
        false
    }

    /// Returns whether the provider replays the plain-text reasoning of
    /// replayed assistant messages, as thinking-mode Kimi and Qwen models
    /// require on replayed tool calls. History another reasoning family
    /// wrote then replays its reasoning notes as reasoning; otherwise, as by
    /// default, as assistant text.
    fn replays_reasoning_text(&self) -> bool {
        false
    }

    /// Returns whether the implementation answers requests in
    /// [`OutputMode::Text`] with plain text, returned as
    /// [`ModelResponse::Text`]. The default, `false`, makes text-mode turns
    /// fail before execution with [`ModelError::UnsupportedOutputMode`];
    /// [`ModelClient`] supports it.
    fn supports_text_output(&self) -> bool {
        false
    }

    /// Checks adapter-specific generation requirements, such as an
    /// output-token bound the provider rejects, without execution.
    /// Implementations accepting every bound may use the default.
    ///
    /// # Errors
    /// Returns [`ModelError::UnsupportedGeneration`] before turn acquisition
    /// for settings the adapter cannot apply.
    fn validate_generation(&self, generation: &GenerationParams) -> Result<(), ModelError> {
        let _ = generation;

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
/// structured output. Its constructors require the provider's `providers-*`
/// feature.
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub struct ModelClient {
    router: ag_router::Router,
    router_model: String,
    lifecycle: LifecycleEmitter,
    metadata: ModelMetadata,
    profile: Option<ModelProfile>,
}

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
impl ModelClient {
    /// Creates a client backed by Moonshot AI's Kimi API.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    #[cfg(feature = "providers-compat")]
    pub fn kimi(config: KimiConfig) -> Result<Self, ModelMetadataError> {
        let profile = provider::kimi_profile(&config.model);
        let model = config.model.clone();

        Self::routed(ProviderSpec::kimi(config), &model, profile)
    }

    /// Creates a client backed by Meta's Model API for Muse models.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    #[cfg(feature = "providers-compat")]
    pub fn muse(config: MuseConfig) -> Result<Self, ModelMetadataError> {
        let profile = provider::muse_profile(&config.model);
        let model = config.model.clone();

        Self::routed(ProviderSpec::muse(config), &model, profile)
    }

    /// Creates a client backed by `OpenAI`'s Responses API. Requests are
    /// streamed and stateless (`store: false`): each carries its whole
    /// history, with the encrypted reasoning of earlier responses replayed
    /// verbatim, and output is constrained by a native JSON Schema format.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    #[cfg(feature = "providers-openai")]
    pub fn openai(config: OpenAiConfig) -> Result<Self, ModelMetadataError> {
        let profile = provider::openai_profile(&config.model);
        let model = config.model.clone();

        Self::routed(ProviderSpec::openai(config), &model, profile)
    }

    /// Creates a client for `OpenAI`'s Responses API billed to the user's
    /// `ChatGPT` plan. Requests carry the access token of the sign-in stored
    /// at `config.auth_file`, refreshed before it expires; otherwise they
    /// match [`Self::openai`].
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    #[cfg(feature = "providers-openai")]
    pub fn chatgpt(config: ChatGptConfig) -> Result<Self, ModelMetadataError> {
        let profile = provider::chatgpt_profile(&config.model);
        let model = config.model.clone();

        Self::routed(ProviderSpec::chatgpt(config), &model, profile)
    }

    /// Creates a client backed by a local or self-hosted server implementing
    /// the `OpenAI` Chat Completions API, such as Ollama, llama.cpp, LM
    /// Studio, or vLLM. Requests carry only portable fields: no prompt cache
    /// key, reasoning controls, or image input, and the output schema is
    /// stated in the prompt. Tool calls must be supported by the server.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    #[cfg(feature = "providers-compat")]
    pub fn openai_compatible(config: OpenAiCompatibleConfig) -> Result<Self, ModelMetadataError> {
        let model = config.model.clone();

        Self::routed(ProviderSpec::openai_compatible(config), &model, None)
    }

    /// Creates a client backed by Alibaba Cloud Model Studio's Qwen API.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    #[cfg(feature = "providers-compat")]
    pub fn qwen(config: QwenConfig) -> Result<Self, ModelMetadataError> {
        let profile = provider::qwen_profile(&config.model);
        let model = config.model.clone();

        Self::routed(ProviderSpec::qwen(config), &model, profile)
    }

    /// Creates a client for the model `spec` serves (a preset's configured
    /// model, or the one [`ProviderSpec::for_model`] names) through any
    /// provider `spec` describes, such as an OpenAI-compatible gateway with
    /// its own headers or query parameters. The client declares no context
    /// profile, even for a preset such as `ProviderSpec::kimi` whose
    /// constructor (`ModelClient::kimi`) attaches the catalog's, so hosts set
    /// the budget explicitly; telemetry and persisted metadata carry
    /// [`ProviderSpec::name`].
    ///
    /// # Errors
    ///
    /// Returns [`ProviderSpecError::Invalid`] when no request can be sent
    /// with `spec` (see [`ProviderSpec::validate`]),
    /// [`ProviderSpecError::MissingModel`] when `spec` names no model, and
    /// [`ProviderSpecError::Metadata`] when the provider name or model
    /// identifier is blank.
    pub fn from_spec(spec: ProviderSpec) -> Result<Self, ProviderSpecError> {
        spec.validate()?;
        let model = spec
            .model()
            .ok_or(ProviderSpecError::MissingModel)?
            .to_string();

        Ok(Self::routed(spec, &model, None)?)
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
        let (result, metadata) = match generated {
            Ok(completion) => {
                let metadata = match &completion {
                    ag_router::Completion::Output { metadata, .. }
                    | ag_router::Completion::Text { metadata, .. }
                    | ag_router::Completion::ToolCalls { metadata, .. } => metadata.clone(),
                };
                (Self::from_router_completion(completion), Some(metadata))
            }
            Err((error, metadata)) => (Err(error), metadata),
        };

        match &result {
            Ok(_) => metrics.completed(metadata.as_ref()),
            Err(error) => metrics.failed(error, metadata.as_ref()),
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
        spec: ProviderSpec,
        model: &str,
        profile: Option<ModelProfile>,
    ) -> Result<Self, ModelMetadataError> {
        let (name, reasoning_family, spec) = spec.into_parts();
        let metadata = ModelMetadata::new(name, model)?.with_reasoning_family(reasoning_family);
        let router_model = format!("{}/{model}", spec.id());
        let router = ag_router::Router::new([spec]);

        Ok(Self {
            router,
            router_model,
            lifecycle: LifecycleEmitter::default(),
            metadata,
            profile,
        })
    }

    fn to_router_request(
        &self,
        request: &ModelRequest,
    ) -> Result<ag_router::ModelRequest, ModelError> {
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
        let mut routed = match request.schema() {
            Some(schema) => ag_router::ModelRequest::chat(
                &self.router_model,
                messages,
                tools,
                ag_router::JsonSchemaFormat::new("ag_harness_output", schema.clone())
                    .map_err(Self::from_router_error)?,
            ),
            None => ag_router::ModelRequest::text_chat(&self.router_model, messages, tools),
        };
        routed.options.max_output_tokens = request.generation().max_output_tokens;
        routed.options.reasoning_effort = request
            .model_reasoning_effort()
            .map(ReasoningEffort::to_router);
        routed
            .options
            .prompt_cache_key
            .clone_from(&request.cache_key);
        routed
            .options
            .prefix_schema
            .clone_from(&request.prefix_schema);
        Ok(routed)
    }

    fn to_router_message(message: &ModelMessage) -> Result<RoutedMessage, ModelError> {
        Ok(match message {
            ModelMessage::Assistant(content) => RoutedMessage::Assistant(content.clone()),
            ModelMessage::AssistantReasoning {
                content,
                reasoning_content,
            } => RoutedMessage::AssistantReasoning {
                content: content.clone(),
                reasoning_content: reasoning_content.clone(),
            },
            ModelMessage::AssistantToolCall(call) => {
                RoutedMessage::AssistantToolCall(Self::to_router_call(call)?)
            }
            ModelMessage::AssistantToolCalls(calls) => RoutedMessage::AssistantToolCalls(
                calls
                    .iter()
                    .map(Self::to_router_call)
                    .collect::<Result<_, _>>()?,
            ),
            ModelMessage::Instructions(content) => RoutedMessage::Instructions(content.clone()),
            ModelMessage::System(content) => RoutedMessage::System(content.clone()),
            ModelMessage::ToolResult {
                call_id,
                content,
                name,
            } => RoutedMessage::ToolResult {
                call_id: call_id.clone(),
                content: content.clone(),
                name: name.clone(),
            },
            ModelMessage::User(content) => RoutedMessage::User(content.clone()),
            ModelMessage::UserInput(input) => {
                RoutedMessage::UserInput(Self::to_router_input(input)?)
            }
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
        .and_then(|routed| routed.with_assistant_text(call.assistant_text().map(str::to_string)))
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
            ag_router::Completion::Text {
                text,
                metadata,
                reasoning_content,
            } => ModelCompletion::new(metadata, ModelResponse::Text(text))
                .with_reasoning_content(reasoning_content),
            ag_router::Completion::ToolCalls { calls, metadata } => {
                let mut calls = Self::from_router_calls(calls)?;
                let response = if calls.len() == 1 {
                    ModelResponse::ToolCall(calls.remove(0))
                } else {
                    ModelResponse::ToolCalls(calls)
                };
                ModelCompletion::new(metadata, response)
            }
        })
    }

    /// Converts router-decoded tool calls into harness tool calls.
    ///
    /// The router already enforces the same identifier, content-size, and
    /// advertised-name rules, so this fails only if the two contracts diverge.
    fn from_router_calls(
        calls: Vec<ag_router::ToolCall>,
    ) -> Result<Vec<tool::ToolCall>, ModelError> {
        calls
            .into_iter()
            .map(|call| {
                tool::ToolCall::from_json(
                    call.id().to_string(),
                    call.name(),
                    &call.arguments_json(),
                    call.reasoning_content().map(str::to_string),
                )?
                .with_assistant_text(call.assistant_text().map(str::to_string))
            })
            .collect()
    }

    fn from_router_error(error: ag_router::ModelError) -> ModelError {
        let error_type = match error.error_class() {
            ag_router::ErrorClass::Request => ModelErrorType::Request,
            ag_router::ErrorClass::Transport => ModelErrorType::Transport,
            ag_router::ErrorClass::Provider => ModelErrorType::Provider,
            ag_router::ErrorClass::ContextOverflow => ModelErrorType::ContextOverflow,
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
        let status = ProviderStatus {
            http_status: error.http_status(),
            quota_exceeded: error.is_quota_exceeded(),
            retry_after: error.retry_after(),
        };
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
            ag_router::ModelError::UnsupportedGeneration { reason } => {
                ModelError::UnsupportedGeneration { reason }
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
            error => ModelError::classified_request(error_type, status, error.into_source()),
        }
    }
}

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
#[async_trait]
impl Model for ModelClient {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(self.metadata.clone())
    }

    fn profile(&self) -> Option<ModelProfile> {
        self.profile
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

    fn keeps_prefix_across_schemas(&self) -> bool {
        self.router
            .keeps_prefix_across_schemas(&self.router_model)
            .unwrap_or(false)
    }

    fn replays_reasoning_text(&self) -> bool {
        self.router
            .replays_reasoning_text(&self.router_model)
            .unwrap_or(false)
    }

    fn supports_text_output(&self) -> bool {
        true
    }

    fn validate_generation(&self, generation: &GenerationParams) -> Result<(), ModelError> {
        let options = ag_router::GenerationOptions {
            max_output_tokens: generation.max_output_tokens,
            ..ag_router::GenerationOptions::default()
        };

        self.router
            .validate_generation(&self.router_model, &options)
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
    provider: Cow<'static, str>,
    reasoning_family: ReasoningFamily,
}

impl ModelMetadata {
    /// Creates metadata for one provider model, in the reasoning family of
    /// its provider name: the two built-in `OpenAI` providers share one
    /// ([`ReasoningFamily::Providers`]), each built-in OpenAI-compatible
    /// model is its own ([`ReasoningFamily::Model`]), and every other
    /// provider name groups its models ([`ReasoningFamily::Provider`]).
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when `provider` or `model` is empty or
    /// contains only whitespace.
    pub fn new(
        provider: impl Into<Cow<'static, str>>,
        model: impl Into<String>,
    ) -> Result<Self, ModelMetadataError> {
        let provider = provider.into();
        if provider.trim().is_empty() {
            return Err(ModelMetadataError::EmptyProvider);
        }
        let model = model.into();
        if model.trim().is_empty() {
            return Err(ModelMetadataError::EmptyModel);
        }
        let reasoning_family = ReasoningFamily::for_provider(&provider);

        Ok(Self {
            model,
            provider,
            reasoning_family,
        })
    }

    /// Returns this metadata in `reasoning_family`.
    #[must_use]
    pub fn with_reasoning_family(mut self, reasoning_family: ReasoningFamily) -> Self {
        self.reasoning_family = reasoning_family;

        self
    }

    /// Returns the model identifier sent to the provider.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the provider identifier used by telemetry.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Returns the reasoning family that decides which recorded turns this
    /// model replays with their provider reasoning intact.
    pub fn reasoning_family(&self) -> &ReasoningFamily {
        &self.reasoning_family
    }

    /// Returns whether a turn the model `model` of the provider named
    /// `provider` recorded belongs to this model's reasoning family.
    pub(crate) fn shares_reasoning_with(&self, provider: &str, model: &str) -> bool {
        let same_provider = provider == self.provider;
        match &self.reasoning_family {
            ReasoningFamily::Provider => same_provider,
            ReasoningFamily::Model => same_provider && model == self.model,
            ReasoningFamily::Providers(providers) => {
                same_provider || providers.iter().any(|name| name == provider)
            }
        }
    }
}

/// Which recorded turns a model replays with their provider reasoning
/// intact: turns of a model in its reasoning family. After a model switch,
/// turns another family recorded replay their reasoning as short notes,
/// because one provider cannot read another's reasoning encoding.
///
/// Recorded turns name only their provider and model, so a family is
/// matched against those names.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ReasoningFamily {
    /// Every model reported under this model's provider name.
    Provider,
    /// Only this model, for a provider name that covers unrelated servers,
    /// such as every OpenAI-compatible server.
    Model,
    /// Every model reported under this model's provider name or one of these
    /// provider names, such as `OpenAI` models reached with an API key and
    /// those billed to a `ChatGPT` plan, which share encrypted `Responses`
    /// reasoning items.
    ///
    /// The list is directional: it decides only which recorded turns this
    /// model replays natively. A model of a listed provider replays this
    /// model's turns natively only when its own family lists this model's
    /// provider too, as the two built-in `OpenAI` providers list each other.
    Providers(Vec<Cow<'static, str>>),
}

impl ReasoningFamily {
    /// Returns the family of the built-in provider named `provider`: the
    /// two `OpenAI` providers share one, each OpenAI-compatible model is its
    /// own, and every other provider name groups its models.
    pub(crate) fn for_provider(provider: &str) -> Self {
        match provider {
            telemetry::PROVIDER_OPENAI => {
                Self::Providers(vec![Cow::Borrowed(telemetry::PROVIDER_OPENAI_CHATGPT)])
            }
            telemetry::PROVIDER_OPENAI_CHATGPT => {
                Self::Providers(vec![Cow::Borrowed(telemetry::PROVIDER_OPENAI)])
            }
            telemetry::PROVIDER_OPENAI_COMPATIBLE => Self::Model,
            _ => Self::Provider,
        }
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
    cache_key: Option<String>,
    generation: GenerationParams,
    history_turn_starts: Vec<usize>,
    input_index: usize,
    lifecycle_observed: bool,
    messages: Vec<ModelMessage>,
    model_reasoning_effort: Option<ReasoningEffort>,
    output: OutputMode,
    prefix_schema: Option<OutputSchema>,
    prompt: String,
    tools: Vec<tool::ToolDefinition>,
}

impl ModelRequest {
    /// Creates a model request whose final response takes the form of
    /// `output`, usually a JSON [`OutputSchema`].
    ///
    /// Plain strings remain the text-only construction path; ordered
    /// text/image content uses an explicit [`TurnInput`].
    pub fn new(input: impl Into<TurnInput>, output: impl Into<OutputMode>) -> Self {
        Self::with_history(Vec::new(), input, output)
    }

    pub(crate) fn with_history(
        messages: Vec<ModelMessage>,
        input: impl Into<TurnInput>,
        output: impl Into<OutputMode>,
    ) -> Self {
        let output = output.into();
        let input = input.into();
        let prompt = input.joined_text();
        let mut messages = messages;
        let input_index = messages.len();
        messages.push(input.into_user_message());

        Self {
            cache_key: None,
            generation: GenerationParams::default(),
            history_turn_starts: Vec::new(),
            input_index,
            lifecycle_observed: false,
            messages,
            model_reasoning_effort: None,
            output,
            prefix_schema: None,
            prompt,
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

    /// Returns the form the final response must take.
    ///
    /// For [`OutputMode::Text`], adapters ask for plain text and return it
    /// as [`ModelResponse::Text`].
    pub fn output(&self) -> &OutputMode {
        &self.output
    }

    /// Returns the schema that the response must match, or `None` when the
    /// final response is plain text.
    pub fn schema(&self) -> Option<&OutputSchema> {
        self.output.schema()
    }

    /// Returns the provider generation parameters of this completion.
    pub fn generation(&self) -> GenerationParams {
        self.generation
    }

    /// Returns the schema of the conversation this request continues when it
    /// asks for another one, as a round summary does. Adapters whose prompt
    /// states the output schema in a leading message state this one there,
    /// so the request keeps the conversation's cached prefix (see
    /// [`Model::keeps_prefix_across_schemas`]); the request's own schema is
    /// then stated in its last message and still validates the response.
    pub fn prefix_schema(&self) -> Option<&OutputSchema> {
        self.prefix_schema.as_ref()
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

    /// Returns the stable key the harness assigns to requests that share a
    /// prompt prefix: one per durable session, or one per stateless turn.
    ///
    /// Adapters may forward it to providers that route prompt-cache hits by
    /// such a key; it carries no conversation content.
    pub fn cache_key(&self) -> Option<&str> {
        self.cache_key.as_deref()
    }

    /// Groups this request with others sharing its prompt prefix.
    pub(crate) fn with_cache_key(mut self, cache_key: String) -> Self {
        self.cache_key = Some(cache_key);

        self
    }

    /// Sets provider generation parameters for this completion.
    pub(crate) fn with_generation(mut self, generation: GenerationParams) -> Self {
        self.generation = generation;

        self
    }

    /// Replaces the requested reasoning depth, clearing it with `None`.
    pub(crate) fn with_optional_reasoning_effort(
        mut self,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Self {
        self.model_reasoning_effort = reasoning_effort;

        self
    }

    /// Places the turn's host `instructions`, if any, right after its input
    /// and before any tool round, so they follow the cached prompt prefix.
    pub(crate) fn with_instructions(mut self, instructions: Option<&str>) -> Self {
        if let Some(instructions) = instructions {
            self.messages.insert(
                self.input_index + 1,
                ModelMessage::Instructions(instructions.to_string()),
            );
        }

        self
    }

    /// Returns the turn's host instructions message, if it carries one.
    pub(crate) fn instructions_message(&self) -> Option<&ModelMessage> {
        self.messages
            .get(self.input_index + 1)
            .filter(|message| matches!(message, ModelMessage::Instructions(_)))
    }

    /// Returns where the current turn's tool rounds start: after its input
    /// and its host instructions, if any.
    pub(crate) fn rounds_start(&self) -> usize {
        self.input_index + 1 + usize::from(self.instructions_message().is_some())
    }

    /// Marks where each projected history turn starts among the leading
    /// messages, so context fitting can drop the oldest turns whole.
    pub(crate) fn with_history_turn_starts(mut self, history_turn_starts: Vec<usize>) -> Self {
        self.history_turn_starts = history_turn_starts;

        self
    }

    /// Returns the ascending message indices where projected history turns
    /// start; empty when the request carries no projected history turns.
    pub(crate) fn history_turn_starts(&self) -> &[usize] {
        &self.history_turn_starts
    }

    /// Returns the index of the current turn's input message.
    pub(crate) fn input_index(&self) -> usize {
        self.input_index
    }

    /// Returns a copy of this request that sends `messages` instead of its
    /// canonical conversation.
    pub(crate) fn with_fitted_messages(&self, messages: Vec<ModelMessage>) -> Self {
        Self {
            cache_key: self.cache_key.clone(),
            generation: self.generation,
            history_turn_starts: Vec::new(),
            input_index: self.input_index,
            lifecycle_observed: self.lifecycle_observed,
            messages,
            model_reasoning_effort: self.model_reasoning_effort,
            output: self.output.clone(),
            prefix_schema: self.prefix_schema.clone(),
            prompt: self.prompt.clone(),
            tools: self.tools.clone(),
        }
    }

    /// Returns a copy of this request that appends `message` and asks for
    /// `schema`, keeping its tools, reasoning depth, and cache key, and
    /// naming its own schema as [`Self::prefix_schema`], so a provider can
    /// serve the copy from this request's cached prefix. The harness
    /// continues only requests for JSON output this way.
    pub(crate) fn continued_with(&self, message: ModelMessage, schema: OutputSchema) -> Self {
        let mut continued = self.with_fitted_messages(self.messages.clone());
        continued.messages.push(message);
        continued.prefix_schema = self
            .prefix_schema
            .clone()
            .or_else(|| self.schema().cloned());
        continued.output = OutputMode::Json(schema);

        continued
    }

    /// Returns this request's messages for in-place maintenance of a fitted
    /// copy.
    pub(crate) fn messages_mut(&mut self) -> &mut Vec<ModelMessage> {
        &mut self.messages
    }

    pub(crate) fn advertises_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name() == name)
    }

    #[cfg(any(feature = "providers-compat", feature = "providers-openai", test))]
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

    /// Appends request-only harness feedback, such as after a rejected final
    /// output or repeated tool calls; it is never journaled.
    pub(crate) fn record_feedback(&mut self, feedback: String) -> &ModelMessage {
        self.record(ModelMessage::User(feedback))
    }

    /// Appends a final answer as assistant `content`: a text answer as its
    /// text, structured output as JSON text.
    pub(crate) fn record_output_with_reasoning(
        &mut self,
        content: String,
        reasoning_content: Option<String>,
    ) {
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

    /// Returns the router's equivalent effort.
    #[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
    pub(crate) const fn to_router(self) -> ag_router::ReasoningEffort {
        match self {
            Self::Low => ag_router::ReasoningEffort::Low,
            Self::Medium => ag_router::ReasoningEffort::Medium,
            Self::High => ag_router::ReasoningEffort::High,
            Self::XHigh => ag_router::ReasoningEffort::XHigh,
            Self::Max => ag_router::ReasoningEffort::Max,
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
    /// Host instructions for one turn, placed right after its input (see
    /// [`crate::TurnOptions::with_instructions`]). Adapters send them as
    /// native developer or system instructions where the provider accepts
    /// them mid-conversation, and otherwise as a user message marked as the
    /// application's.
    Instructions(String),
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
            Self::Assistant(content)
            | Self::Instructions(content)
            | Self::System(content)
            | Self::User(content) => content.len(),
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
                    .saturating_add(call.assistant_text().map_or(0, str::len))
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
                    .saturating_add(call.assistant_text().map_or(0, str::len))
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

    #[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
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
    /// Terminal structured model output answering a request in
    /// [`OutputMode::Json`].
    ///
    /// [`ModelClient`] and [`crate::Harness`] validate this value locally
    /// against the request schema before returning it to applications; the
    /// harness rejects it for a text request.
    Output(Value),
    /// Terminal plain-text answer to a request in [`OutputMode::Text`].
    ///
    /// The harness requires nonblank text within the output size bound and
    /// rejects it for a JSON request.
    Text(String),
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
            Self::Text(_) | Self::ToolCall(_) | Self::ToolCalls(_) => None,
        }
    }

    /// Returns the terminal plain-text answer, when present.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Output(_) | Self::ToolCall(_) | Self::ToolCalls(_) => None,
        }
    }

    /// Returns the intermediate native function call, when present.
    pub fn call(&self) -> Option<&tool::ToolCall> {
        match self {
            Self::ToolCall(call) => Some(call),
            Self::Output(_) | Self::Text(_) | Self::ToolCalls(_) => None,
        }
    }

    /// Returns every intermediate native function call in this response.
    pub fn calls(&self) -> &[tool::ToolCall] {
        match self {
            Self::Output(_) | Self::Text(_) => &[],
            Self::ToolCall(call) => std::slice::from_ref(call),
            Self::ToolCalls(calls) => calls,
        }
    }

    pub(crate) fn response_type(&self) -> ModelResponseType {
        match self {
            Self::Output(_) | Self::Text(_) => ModelResponseType::Output,
            Self::ToolCall(_) | Self::ToolCalls(_) => ModelResponseType::ToolCall,
        }
    }
}

ag_router::define_model_response_error! {
    /// Failure returned while completing a model request.
    pub enum ModelError {
        /// The provider returned more than the single supported call.
        #[error("model returned multiple tool calls")]
        MultipleToolCalls,
        /// The model cannot answer in the requested output mode, such as
        /// plain text.
        #[error("model does not support this output mode: {reason}")]
        UnsupportedOutputMode {
            /// Model limitation.
            reason: String,
        },
    }
}

impl ModelError {
    /// Wraps a provider transport or response-decoding failure.
    pub fn request(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Request(Box::new(error))
    }

    /// Wraps a provider failure that rejected the request because its input
    /// exceeds the model's context window, so custom [`Model`]s report the
    /// same [`ModelErrorType::ContextOverflow`] as built-in providers.
    pub fn context_overflow(error: impl Error + Send + Sync + 'static) -> Self {
        Self::classified_request(
            ModelErrorType::ContextOverflow,
            ProviderStatus::default(),
            Box::new(error),
        )
    }

    /// Wraps an unsuccessful provider HTTP response, so custom [`Model`]s
    /// report the same status and `Retry-After` classification as built-in
    /// providers.
    pub fn provider(
        http_status: u16,
        retry_after: Option<Duration>,
        error: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self::classified_request(
            ModelErrorType::Provider,
            ProviderStatus {
                http_status: Some(http_status),
                quota_exceeded: false,
                retry_after,
            },
            Box::new(error),
        )
    }

    /// Wraps a provider failure reporting a spent quota or credit balance,
    /// so custom [`Model`]s report the same non-retryable failure as
    /// built-in providers.
    pub fn quota_exceeded(error: impl Error + Send + Sync + 'static) -> Self {
        Self::classified_request(
            ModelErrorType::Provider,
            ProviderStatus {
                http_status: None,
                quota_exceeded: true,
                retry_after: None,
            },
            Box::new(error),
        )
    }

    /// Returns whether the provider rejected the request because its input
    /// exceeds the model's context window.
    pub fn is_context_overflow(&self) -> bool {
        self.error_type() == ModelErrorType::ContextOverflow
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
            Self::UnsupportedOutputSchema { .. }
            | Self::UnsupportedOutputMode { .. }
            | Self::UnsupportedGeneration { .. } => ModelErrorType::UnsupportedOutput,
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
        self.provider_status().and_then(|status| status.http_status)
    }

    /// Returns the provider-declared `Retry-After` delay of an unsuccessful
    /// HTTP response, when one was received. Built-in providers honor only
    /// delta-seconds values, capped at sixty seconds.
    pub fn retry_after(&self) -> Option<Duration> {
        self.provider_status().and_then(|status| status.retry_after)
    }

    /// Returns whether the provider reported a spent quota or credit
    /// balance, which waiting does not fix.
    pub fn is_quota_exceeded(&self) -> bool {
        self.provider_status()
            .is_some_and(|status| status.quota_exceeded)
    }

    pub(crate) fn classified_request(
        error_type: ModelErrorType,
        status: ProviderStatus,
        source: Box<dyn Error + Send + Sync>,
    ) -> Self {
        Self::Request(Box::new(ClassifiedRequestError {
            error_type,
            source,
            status,
        }))
    }

    fn provider_status(&self) -> Option<&ProviderStatus> {
        match self {
            Self::Request(source) => source
                .downcast_ref::<ClassifiedRequestError>()
                .map(|error| &error.status),
            _ => None,
        }
    }
}

/// Provider response details retained with a classified request failure.
#[derive(Debug, Default)]
pub(crate) struct ProviderStatus {
    /// HTTP status of the unsuccessful provider response.
    pub(crate) http_status: Option<u16>,
    /// The provider reported a spent quota or credit balance.
    pub(crate) quota_exceeded: bool,
    /// Provider-declared `Retry-After` delay.
    pub(crate) retry_after: Option<Duration>,
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
    /// The provider rejected the request because its input exceeds the
    /// model's context window; a smaller request may succeed.
    ContextOverflow,
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
            Self::ContextOverflow => telemetry::ERROR_CONTEXT_OVERFLOW,
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
    #[source]
    source: Box<dyn Error + Send + Sync>,
    status: ProviderStatus,
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
