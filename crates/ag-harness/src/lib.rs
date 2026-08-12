//! Lightweight, Rust-native LLM harness for application-facing agent workflows.
//!
//! The crate provides a provider-neutral model client, validated structured
//! output, and private provider backends.

mod chat_completion;
mod model;
mod provider;
mod schema_contract;
mod telemetry;
mod tool;

pub use model::{
    Model, ModelClient, ModelError, ModelMetadata, ModelMetadataError, ModelRequest, ModelResponse,
};
pub use provider::{
    KimiConfig, MUSE_SPARK_1_2, MUSE_SPARK_1_2_CONTRIBUTOR, MuseConfig, QwenConfig,
};
pub use schema_contract::{OutputSchema, OutputSchemaError};
pub use tool::{ReadArguments, ToolCall, ToolDefinition};
