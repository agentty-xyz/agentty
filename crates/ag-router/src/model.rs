//! Provider-neutral model request and completion types.

use std::collections::HashSet;
use std::error::Error;

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

/// Validated JSON Schema output contract. This is the only response format.
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GenerationOptions {
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
    /// Required JSON Schema output contract.
    pub response_format: JsonSchemaFormat,
    /// Generation settings.
    pub options: GenerationOptions,
}

impl ModelRequest {
    /// Creates a chat request with a required JSON Schema format.
    pub fn chat(
        model: impl Into<String>,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolDefinition>,
        response_format: JsonSchemaFormat,
    ) -> Self {
        Self {
            model: model.into(),
            task: Task::Chat(ChatRequest { messages, tools }),
            response_format,
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

    /// Returns the required JSON Schema.
    pub fn schema(&self) -> &OutputSchema {
        self.response_format.schema()
    }

    /// Returns requested reasoning effort.
    pub fn model_reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.options.reasoning_effort
    }

    pub(crate) fn advertises_tool(&self, name: &str) -> bool {
        self.tools().iter().any(|tool| tool.name() == name)
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

/// Final JSON output or intermediate function calls.
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
    /// Function calls requiring caller execution.
    ToolCalls {
        /// Validated function calls.
        calls: Vec<ToolCall>,
        /// Provider response metadata.
        metadata: CompletionMetadata,
    },
}

/// Failure to complete a model request.
#[derive(Debug, Error)]
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
    /// No configuration exists for the requested provider.
    #[error("requested provider is not configured")]
    UnknownProvider,
    /// A provider was configured more than once.
    #[error("provider is configured more than once")]
    DuplicateProvider,
    /// Provider credentials or URL are blank.
    #[error("provider API key and base URL must be nonempty")]
    InvalidProviderConfig,
    /// Request construction or transport failed.
    #[error("model request failed: {0}")]
    Request(#[source] Box<dyn Error + Send + Sync>),
    /// No usable assistant response was returned.
    #[error("model returned no response content")]
    InvalidResponse,
    /// Provider response ended before completion.
    #[error("model response is incomplete: {reason}")]
    IncompleteResponse {
        /// Provider finish reason.
        reason: String,
    },
    /// Response body exceeded its bound.
    #[error("model response body exceeds the size limit")]
    ResponseBodyTooLarge,
    /// The provider cannot use the requested schema.
    #[error("provider cannot satisfy this output schema: {reason}")]
    UnsupportedOutputSchema {
        /// Provider limitation.
        reason: String,
    },
    /// The provider cannot use the requested image input.
    #[error("model does not support image input: {reason}")]
    UnsupportedImageInput {
        /// Model limitation.
        reason: String,
    },
    /// Response content exceeded its bound.
    #[error("model response content exceeds the size limit")]
    ResponseContentTooLarge,
    /// Provider returned invalid JSON.
    #[error("model returned invalid JSON: {reason}")]
    InvalidJson {
        /// Bounded parser diagnostic.
        reason: String,
    },
    /// Provider output did not match the requested schema.
    #[error("model output violates the schema at {path}: {reason}")]
    SchemaViolation {
        /// Path to the invalid value.
        path: String,
        /// Bounded validator diagnostic.
        reason: String,
    },
    /// The provider did not include a required function call.
    #[error("model returned no tool call")]
    MissingToolCall,
    /// A function call identifier was invalid.
    #[error("model returned a blank or oversized tool call identifier")]
    InvalidToolCallId,
    /// Function call identifiers were not unique.
    #[error("model returned duplicate tool call identifier: {id}")]
    DuplicateToolCallId {
        /// Duplicate identifier.
        id: String,
    },
    /// Terminal output also contained function calls.
    #[error("model terminal response contained tool calls")]
    TerminalResponseWithToolCalls,
    /// Provider returned a non-function tool call.
    #[error("model requested unsupported tool type: {kind}")]
    UnsupportedToolType {
        /// Unsupported wire type.
        kind: String,
    },
    /// Provider returned an unadvertised function.
    #[error("model requested unsupported tool: {name}")]
    UnsupportedToolName {
        /// Function name.
        name: String,
    },
    /// Function arguments were invalid.
    #[error("model returned invalid tool arguments: {reason}")]
    InvalidToolArguments {
        /// Bounded parser diagnostic.
        reason: String,
    },
    /// JSON Schema response format name was invalid.
    #[error("response format name must contain 1-128 bytes")]
    InvalidResponseFormatName,
}

impl ModelError {
    /// Returns a stable category for telemetry and callers.
    pub fn error_class(&self) -> ErrorClass {
        match self {
            Self::WithMetadata { source, .. } => source.error_class(),
            Self::Request(source) => {
                if source.downcast_ref::<ProviderRequestError>().is_some() {
                    ErrorClass::Provider
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
            Self::UnsupportedOutputSchema { .. } => ErrorClass::UnsupportedOutput,
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
            | Self::DuplicateProvider
            | Self::InvalidProviderConfig
            | Self::InvalidResponseFormatName => ErrorClass::Request,
        }
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
        provider: &'static str,
        body: String,
        source: reqwest::Error,
        status: reqwest::StatusCode,
    ) -> Self {
        Self::Request(Box::new(ProviderRequestError {
            provider,
            body,
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
            ModelErrorType::InvalidProviderResponse => ErrorClass::InvalidProviderResponse,
            ModelErrorType::Transport => ErrorClass::Transport,
        };
        Self::Request(Box::new(ClassifiedRequestError {
            class,
            http_status,
            source,
        }))
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
    InvalidProviderResponse,
    Transport,
}

#[derive(Debug, Error)]
#[error("{provider} returned HTTP {status}: {body}")]
struct ProviderRequestError {
    provider: &'static str,
    body: String,
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
