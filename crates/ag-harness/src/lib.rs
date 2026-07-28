//! Lightweight, Rust-native LLM harness for application-facing agent workflows.
//!
//! The crate provides a provider-neutral model contract, validated structured
//! output, and model adapters.

mod model;
mod qwen;
mod schema_contract;

pub use model::{Model, ModelError, ModelRequest, ModelResponse};
pub use qwen::{Qwen, QwenConfig};
pub use schema_contract::{OutputSchema, OutputSchemaError};
