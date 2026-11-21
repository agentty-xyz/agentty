//! Provider-neutral routing for structured LLM chat requests over the Chat
//! Completions, `OpenAI` Responses, and Anthropic Messages wire APIs.
//!
//! A request names a `provider/model`, supplies a JSON Schema response format,
//! and receives either locally validated JSON or function calls for the caller
//! to execute. Each provider is a host-supplied [`ProviderSpec`]: a wire
//! protocol, endpoint, credentials, and request policy, with the built-in
//! providers available as [`preset`]s.

mod anthropic;
mod chat_completion;
pub mod chatgpt;
mod credential;
mod http;
mod input;
mod local_server;
mod model;
pub mod preset;
mod provider;
mod provider_spec;
mod reasoning;
mod response_error;
mod responses;
mod retry;
mod schema;
mod stream;
mod tool;

pub use credential::CredentialSource;
pub use input::{ImageContent, ImageMediaType, InputBlock, InputError, TurnInput};
pub use local_server::{
    LocalInventory, LocalModel, LocalServer, LocalServerError, LocalServerKind,
};
pub use model::{
    ChatRequest, Completion, CompletionMetadata, CompletionUsage, ErrorClass, GenerationOptions,
    JsonSchemaFormat, ModelError, ModelMessage, ModelRequest, ReasoningEffort, Task,
};
pub use provider::Router;
pub use provider_spec::{
    Auth, EffortNames, InstructionPlacement, MaxTokensField, ProviderSpec, ReasoningMapping,
    StructuredOutputMode, ThinkingOptions, WireProtocol,
};
pub use reasoning::{ReasoningView, reasoning_view};
pub use retry::{Jitter, RetryKind, RetryNotice, RetryObserver, RetryPolicy};
pub use schema::{
    OutputSchema, OutputSchemaError, OutputValidationError, RESPONSE_CONTENT_LIMIT_BYTES,
    bounded_diagnostic, ensure_content_size,
};
pub use stream::{DeltaObserver, ResponseDelta};
pub use tool::{ToolCall, ToolDefinition, is_valid_function_name, replayable_function_name};

/// Compiles the README's Rust examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
