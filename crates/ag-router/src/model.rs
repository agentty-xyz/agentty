//! Provider-neutral model request and completion types.

use std::collections::HashSet;
use std::error::Error;
use std::num::NonZeroU32;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::input::TurnInput;
use crate::schema::{self, OutputSchema, OutputValidationError};
use crate::tool::{ToolCall, ToolDefinition};

/// Requested depth of provider reasoning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReasoningEffort {
    /// Light reasoning.
    Low,
    /// Moderate reasoning.
    Medium,
    /// Deep reasoning.
    High,
    /// Extra-high reasoning.
    XHigh,
    /// Maximum supported reasoning.
    Max,
}

impl ReasoningEffort {
    /// Returns the provider-neutral name.
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

/// One message in a chat request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelMessage {
    /// Assistant text from a completed turn.
    Assistant(String),
    /// Assistant text and provider reasoning required for replay.
    AssistantReasoning {
        /// Assistant text.
        content: String,
        /// Provider-specific reasoning content.
        reasoning_content: String,
    },
    /// One assistant function call.
    AssistantToolCall(ToolCall),
    /// Assistant function calls in one response.
    AssistantToolCalls(Vec<ToolCall>),
    /// Host instructions for one turn, placed after the conversation prefix
    /// so changing them keeps that prefix cacheable. The Responses API
    /// receives them as a `developer` message. Chat Completions appends them,
    /// under an `[application instructions for this turn]` heading, to the
    /// user message they follow, since many chat templates accept system
    /// messages only first and require user and assistant turns to
    /// alternate.
    Instructions(String),
    /// System instructions.
    System(String),
    /// Result of one prior function call.
    ToolResult {
        /// Function call identifier.
        call_id: String,
        /// Serialized result content.
        content: String,
        /// Function name.
        name: String,
    },
    /// Plain-text user message.
    User(String),
    /// Ordered text and image user content.
    UserInput(TurnInput),
}

/// Chat request payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatRequest {
    /// Conversation history, including the current user input.
    pub messages: Vec<ModelMessage>,
    /// Functions advertised to the model.
    pub tools: Vec<ToolDefinition>,
}

/// Model operation selected by the request.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Task {
    /// Chat generation with optional function calls.
    Chat(ChatRequest),
}

/// Validated JSON Schema output contract of a structured request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonSchemaFormat {
    name: String,
    schema: OutputSchema,
}

impl JsonSchemaFormat {
    /// Binds a validated schema to a wire-format name.
    ///
    /// # Errors
    /// Returns an error when the name is empty or too long.
    pub fn new(name: impl Into<String>, schema: OutputSchema) -> Result<Self, ModelError> {
        let name = name.into();
        if name.trim().is_empty() || name.len() > 128 {
            return Err(ModelError::InvalidResponseFormatName);
        }

        Ok(Self { name, schema })
    }

    /// Returns the wire-format name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the validated JSON Schema.
    pub fn schema(&self) -> &OutputSchema {
        &self.schema
    }
}

/// Optional generation settings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GenerationOptions {
    /// Upper bound on generated tokens, reasoning included, sent as
    /// `max_output_tokens` to the Responses API and as the provider's
    /// `max_tokens` or `max_completion_tokens` to Chat Completions. A
    /// provider that rejects the bound, such as the ChatGPT-plan backend or
    /// the Responses API below its minimum of 16, fails the request with
    /// [`ModelError::UnsupportedGeneration`] before sending it (see
    /// [`crate::Router::validate_generation`]).
    pub max_output_tokens: Option<NonZeroU32>,
    /// Output schema of the conversation this request continues, when the
    /// request asks for a different one. Providers that state the output
    /// schema in a leading prompt message (see
    /// [`crate::Router::keeps_prefix_across_schemas`]) state this one there
    /// instead, so the request keeps the conversation's cached prefix; the
    /// caller then states the request's own schema after the conversation.
    /// Every provider still validates the response against the request's own
    /// schema, and providers that constrain output natively send only that
    /// one.
    pub prefix_schema: Option<OutputSchema>,
    /// Stable key grouping requests that share a prompt prefix, sent as
    /// `prompt_cache_key` only to providers that route prompt-cache hits by it.
    pub prompt_cache_key: Option<String>,
    /// Requested reasoning depth, if supported by the model.
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// One provider-neutral request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    /// Explicit `provider/model` identifier; the model may contain `/`.
    pub model: String,
    /// Model operation.
    pub task: Task,
    /// JSON Schema output contract, or `None` for a plain-text final answer.
    pub response_format: Option<JsonSchemaFormat>,
    /// Generation settings.
    pub options: GenerationOptions,
}

impl ModelRequest {
    /// Creates a chat request whose final answer must match a JSON Schema.
    pub fn chat(
        model: impl Into<String>,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolDefinition>,
        response_format: JsonSchemaFormat,
    ) -> Self {
        Self {
            model: model.into(),
            task: Task::Chat(ChatRequest { messages, tools }),
            response_format: Some(response_format),
            options: GenerationOptions::default(),
        }
    }

    /// Creates a chat request whose final answer is plain text, returned as
    /// [`Completion::Text`].
    pub fn text_chat(
        model: impl Into<String>,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Self {
        Self {
            model: model.into(),
            task: Task::Chat(ChatRequest { messages, tools }),
            response_format: None,
            options: GenerationOptions::default(),
        }
    }

    /// Returns the conversation messages.
    pub fn messages(&self) -> &[ModelMessage] {
        match &self.task {
            Task::Chat(chat) => &chat.messages,
        }
    }

    /// Returns advertised function definitions.
    pub fn tools(&self) -> &[ToolDefinition] {
        match &self.task {
            Task::Chat(chat) => &chat.tools,
        }
    }

    /// Returns the JSON Schema of a structured request, or `None` when the
    /// final answer is plain text.
    pub fn schema(&self) -> Option<&OutputSchema> {
        self.response_format.as_ref().map(JsonSchemaFormat::schema)
    }

    /// Returns requested reasoning effort.
    pub fn model_reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.options.reasoning_effort
    }
}

/// Provider response metadata.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CompletionMetadata {
    finish_reason: String,
    response_id: Option<String>,
    response_model: Option<String>,
    system_fingerprint: Option<String>,
    usage: Option<CompletionUsage>,
}

impl CompletionMetadata {
    /// Creates metadata from a provider completion.
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

    /// Returns the normalized finish reason.
    pub fn finish_reason(&self) -> &str {
        &self.finish_reason
    }

    /// Returns the provider response identifier.
    pub fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    /// Returns the resolved response model.
    pub fn response_model(&self) -> Option<&str> {
        self.response_model.as_deref()
    }

    /// Returns the provider fingerprint, if reported.
    pub fn system_fingerprint(&self) -> Option<&str> {
        self.system_fingerprint.as_deref()
    }

    /// Returns token usage, if reported.
    pub fn usage(&self) -> Option<&CompletionUsage> {
        self.usage.as_ref()
    }
}

/// Provider-reported token counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CompletionUsage {
    cache_hit: Option<u64>,
    cache_miss: Option<u64>,
    input: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
    total: Option<u64>,
}

impl CompletionUsage {
    /// Creates normalized token counts.
    pub fn new(
        cache_hit: Option<u64>,
        cache_miss: Option<u64>,
        input: Option<u64>,
        output: Option<u64>,
        reasoning: Option<u64>,
        total: Option<u64>,
    ) -> Self {
        Self {
            cache_hit,
            cache_miss,
            input,
            output,
            reasoning,
            total,
        }
    }

    /// Returns cached input tokens.
    pub fn cache_hit_tokens(self) -> Option<u64> {
        self.cache_hit
    }

    /// Returns non-cached input tokens.
    pub fn cache_miss_tokens(self) -> Option<u64> {
        self.cache_miss
    }

    /// Returns all input tokens.
    pub fn input_tokens(self) -> Option<u64> {
        self.input
    }

    /// Returns output tokens.
    pub fn output_tokens(self) -> Option<u64> {
        self.output
    }

    /// Returns reasoning tokens.
    pub fn reasoning_tokens(self) -> Option<u64> {
        self.reasoning
    }

    /// Returns total tokens.
    pub fn total_tokens(self) -> Option<u64> {
        self.total
    }
}

/// Final output or intermediate function calls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Completion {
    /// Schema-validated terminal output.
    Output {
        /// Validated JSON value.
        value: Value,
        /// Provider response metadata.
        metadata: CompletionMetadata,
        /// Provider reasoning required for continuation.
        reasoning_content: Option<String>,
    },
    /// Plain-text terminal output of a request without a response format.
    Text {
        /// Assistant text, bounded like structured output.
        text: String,
        /// Provider response metadata.
        metadata: CompletionMetadata,
        /// Provider reasoning required for continuation.
        reasoning_content: Option<String>,
    },
    /// Function calls requiring caller execution.
    ToolCalls {
        /// Validated function calls.
        calls: Vec<ToolCall>,
        /// Provider response metadata.
        metadata: CompletionMetadata,
    },
}

crate::define_model_response_error! {
    /// Failure to complete a model request.
    pub enum ModelError {
        /// Failure associated with a decoded provider completion.
        #[error("{source}")]
        WithMetadata {
            /// Metadata retained for usage and telemetry on failed responses.
            metadata: Box<CompletionMetadata>,
            /// Classified response error.
            #[source]
            source: Box<ModelError>,
        },
        /// The model identifier is not `provider/model`.
        #[error("model identifier must be provider/model")]
        InvalidModelId,
        /// No specification serves the requested provider and model.
        #[error("requested provider or model is not configured")]
        UnknownProvider,
        /// The provider specification cannot send a request.
        #[error("provider specification is invalid: {reason}")]
        InvalidProviderConfig {
            /// The field that failed and why.
            reason: String,
        },
        /// JSON Schema response format name was invalid.
        #[error("response format name must contain 1-128 bytes")]
        InvalidResponseFormatName,
    }
}

impl ModelError {
    /// Returns a stable category for telemetry and callers.
    pub fn error_class(&self) -> ErrorClass {
        match self {
            Self::WithMetadata { source, .. } => source.error_class(),
            Self::Request(source) => {
                if let Some(error) = source.downcast_ref::<ProviderRequestError>() {
                    if error.rejection.context_overflow {
                        ErrorClass::ContextOverflow
                    } else {
                        ErrorClass::Provider
                    }
                } else {
                    source
                        .downcast_ref::<ClassifiedRequestError>()
                        .map_or(ErrorClass::Request, |error| error.class)
                }
            }
            Self::InvalidResponse | Self::IncompleteResponse { .. } => ErrorClass::InvalidResponse,
            Self::ResponseBodyTooLarge | Self::ResponseContentTooLarge => {
                ErrorClass::ResponseTooLarge
            }
            Self::UnsupportedOutputSchema { .. } | Self::UnsupportedGeneration { .. } => {
                ErrorClass::UnsupportedOutput
            }
            Self::UnsupportedImageInput { .. } => ErrorClass::UnsupportedInput,
            Self::InvalidJson { .. } | Self::SchemaViolation { .. } => ErrorClass::InvalidOutput,
            Self::MissingToolCall
            | Self::InvalidToolCallId
            | Self::DuplicateToolCallId { .. }
            | Self::TerminalResponseWithToolCalls
            | Self::UnsupportedToolType { .. }
            | Self::UnsupportedToolName { .. }
            | Self::InvalidToolArguments { .. } => ErrorClass::InvalidToolCall,
            Self::InvalidModelId
            | Self::UnknownProvider
            | Self::InvalidProviderConfig { .. }
            | Self::InvalidResponseFormatName => ErrorClass::Request,
        }
    }

    /// Returns whether the provider rejected the request because its input
    /// exceeds the model's context window, so a smaller request may succeed.
    pub fn is_context_overflow(&self) -> bool {
        self.error_class() == ErrorClass::ContextOverflow
    }

    /// Returns the provider HTTP status when the request reached a provider.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::WithMetadata { source, .. } => source.http_status(),
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

    /// Returns the provider-declared `Retry-After` delay of an unsuccessful
    /// HTTP response, when one was sent as whole delta-seconds. The delay is
    /// capped at sixty seconds; an HTTP-date value reads as absent.
    pub fn retry_after(&self) -> Option<Duration> {
        self.provider_rejection()
            .and_then(|rejection| rejection.retry_after)
    }

    /// Returns whether the provider reported a spent quota or credit
    /// balance, which waiting does not fix: an HTTP `402`, or a `429` whose
    /// body carries a known quota marker.
    pub fn is_quota_exceeded(&self) -> bool {
        self.provider_rejection()
            .is_some_and(|rejection| rejection.quota_exceeded)
    }

    /// Consumes the error, preserving its underlying request source when
    /// present.
    pub fn into_source(self) -> Box<dyn Error + Send + Sync> {
        match self {
            Self::WithMetadata { source, .. } => source.into_source(),
            Self::Request(source) => source,
            error => Box::new(error),
        }
    }

    /// Returns metadata from a decoded completion that failed validation.
    pub fn metadata(&self) -> Option<&CompletionMetadata> {
        match self {
            Self::WithMetadata { metadata, .. } => Some(metadata.as_ref()),
            _ => None,
        }
    }

    /// Returns the underlying classified error without its metadata wrapper.
    pub fn root_cause(&self) -> &Self {
        match self {
            Self::WithMetadata { source, .. } => source.root_cause(),
            error => error,
        }
    }

    pub(crate) fn with_metadata(self, metadata: CompletionMetadata) -> Self {
        Self::WithMetadata {
            metadata: Box::new(metadata),
            source: Box::new(self),
        }
    }

    pub(crate) fn request(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Request(Box::new(error))
    }

    pub(crate) fn provider_request(
        provider: &str,
        body: String,
        source: reqwest::Error,
        status: reqwest::StatusCode,
        rejection: ProviderRejection,
    ) -> Self {
        Self::Request(Box::new(ProviderRequestError {
            provider: provider.to_string(),
            body,
            rejection,
            source,
            status,
        }))
    }

    pub(crate) fn classified_request(
        error_type: ModelErrorType,
        http_status: Option<u16>,
        source: Box<dyn Error + Send + Sync>,
    ) -> Self {
        let class = match error_type {
            ModelErrorType::ContextOverflow => ErrorClass::ContextOverflow,
            ModelErrorType::InvalidProviderResponse => ErrorClass::InvalidProviderResponse,
            ModelErrorType::Provider => ErrorClass::Provider,
            ModelErrorType::Transport => ErrorClass::Transport,
        };
        Self::Request(Box::new(ClassifiedRequestError {
            class,
            http_status,
            source,
        }))
    }

    fn provider_rejection(&self) -> Option<&ProviderRejection> {
        match self {
            Self::WithMetadata { source, .. } => source.provider_rejection(),
            Self::Request(source) => source
                .downcast_ref::<ProviderRequestError>()
                .map(|error| &error.rejection),
            _ => None,
        }
    }
}

/// Stable failure category for provider-independent telemetry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorClass {
    /// Invalid request or client-side construction failure.
    Request,
    /// Network transport failed.
    Transport,
    /// Provider returned an unsuccessful HTTP response.
    Provider,
    /// Provider rejected the request because its input exceeds the model's
    /// context window.
    ContextOverflow,
    /// Provider response envelope was malformed.
    InvalidProviderResponse,
    /// Provider returned an unusable successful response.
    InvalidResponse,
    /// Schema is unsupported by this provider.
    UnsupportedOutput,
    /// Input modality is unsupported by this model.
    UnsupportedInput,
    /// Response exceeded a safety limit.
    ResponseTooLarge,
    /// Terminal JSON failed parsing or schema validation.
    InvalidOutput,
    /// Function call was missing, invalid, or unsupported.
    InvalidToolCall,
}

/// Stable failure class used by transport adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelErrorType {
    /// A streamed response failed because its input exceeds the window.
    ContextOverflow,
    InvalidProviderResponse,
    /// A streamed response reported a provider failure after its `200`.
    Provider,
    Transport,
}

/// Provider-neutral result of decoding one provider response.
pub(crate) enum GeneratedResponse {
    Failed {
        error: ModelError,
        metadata: CompletionMetadata,
    },
    Output {
        metadata: CompletionMetadata,
        output: String,
        reasoning_content: Option<String>,
    },
    ToolCall {
        call: ToolCall,
        metadata: CompletionMetadata,
    },
    ToolCalls {
        calls: Vec<ToolCall>,
        metadata: CompletionMetadata,
    },
}

impl GeneratedResponse {
    pub(crate) fn failed(error: ModelError, metadata: CompletionMetadata) -> Self {
        Self::Failed { error, metadata }
    }

    /// Wraps decoded calls, keeping a single call as [`Self::ToolCall`].
    pub(crate) fn tool_calls(mut calls: Vec<ToolCall>, metadata: CompletionMetadata) -> Self {
        if calls.len() == 1 {
            Self::ToolCall {
                call: calls.remove(0),
                metadata,
            }
        } else {
            Self::ToolCalls { calls, metadata }
        }
    }
}

/// Classification of an unsuccessful provider HTTP response, read from its
/// status, headers, and bounded body.
#[derive(Debug)]
pub(crate) struct ProviderRejection {
    /// The input exceeded the model's context window.
    pub(crate) context_overflow: bool,
    /// The account's quota or credit balance is spent.
    pub(crate) quota_exceeded: bool,
    /// Provider-declared `Retry-After` delay.
    pub(crate) retry_after: Option<Duration>,
}

#[derive(Debug, Error)]
#[error("{provider} returned HTTP {status}: {body}")]
struct ProviderRequestError {
    provider: String,
    body: String,
    rejection: ProviderRejection,
    #[source]
    source: reqwest::Error,
    status: reqwest::StatusCode,
}

#[derive(Debug, Error)]
#[error("{source}")]
struct ClassifiedRequestError {
    class: ErrorClass,
    http_status: Option<u16>,
    #[source]
    source: Box<dyn Error + Send + Sync>,
}

pub(crate) fn ensure_unique_tool_call_ids(calls: &[ToolCall]) -> Result<(), ModelError> {
    let mut ids = HashSet::with_capacity(calls.len());
    for call in calls {
        if !ids.insert(call.id()) {
            return Err(ModelError::DuplicateToolCallId {
                id: schema::bounded_diagnostic(call.id()),
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
