//! Provider-neutral routing for structured LLM chat requests.
//!
//! A request names a `provider/model`, supplies a JSON Schema response format,
//! and receives either locally validated JSON or function calls for the caller
//! to execute. Provider credentials are supplied by the host.

mod chat_completion;
mod input;
mod model;
mod provider;
mod schema;
mod tool;

pub use input::{ImageContent, ImageMediaType, InputBlock, InputError, TurnInput};
pub use model::{
    ChatRequest, Completion, CompletionMetadata, CompletionUsage, ErrorClass, GenerationOptions,
    JsonSchemaFormat, ModelError, ModelMessage, ModelRequest, ReasoningEffort, Task,
};
pub use provider::{Provider, ProviderConfig, Router};
pub use schema::{
    OutputSchema, OutputSchemaError, OutputValidationError, RESPONSE_CONTENT_LIMIT_BYTES,
    bounded_diagnostic, ensure_content_size,
};
pub use tool::{ToolCall, ToolDefinition};
