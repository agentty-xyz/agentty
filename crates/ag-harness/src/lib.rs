//! Lightweight, Rust-native LLM harness for application-facing agent workflows.
//!
//! The crate provides a provider-neutral model contract, validated structured
//! output, and model adapters.

mod chat_completion;
mod model;
mod provider;
mod schema_contract;
mod telemetry;

pub use model::{Model, ModelBackend, ModelError, ModelMetadata, ModelRequest, ModelResponse};
pub use provider::{Kimi, KimiConfig, Qwen, QwenConfig};
pub use schema_contract::{OutputSchema, OutputSchemaError};
