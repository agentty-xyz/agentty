//! Lightweight, Rust-native LLM harness for application-facing agent workflows.
//!
//! The crate provides a provider-neutral model client, normalized completion
//! metadata, validated structured output, and private provider backends.

mod chat_completion;
mod file_system;
mod harness;
mod lifecycle;
mod model;
mod policy;
mod provider;
mod read;
mod schema_contract;
mod telemetry;
mod tool;

pub use file_system::{FileSystem, LocalFileSystem};
pub use harness::{Harness, TurnError};
pub use lifecycle::{
    LifecycleEvent, LifecycleEventKind, LifecycleId, LifecycleObserver, ModelResponseType,
    ToolErrorType, TurnErrorType,
};
pub use model::{
    CompletionMetadata, CompletionUsage, Model, ModelClient, ModelCompletion, ModelError,
    ModelErrorType, ModelMetadata, ModelMetadataError, ModelRequest, ModelResponse,
    ModelWithMetadata,
};
pub use provider::{
    KimiConfig, MUSE_SPARK_1_2, MUSE_SPARK_1_2_CONTRIBUTOR, Muse, MuseConfig, MuseError, QwenConfig,
};
pub use read::{ReadError, ReadOutput};
pub use schema_contract::{OutputSchema, OutputSchemaError};
pub use tool::{ReadArguments, Tool, ToolCall, ToolDefinition};
