//! Lightweight, Rust-native LLM harness for application-facing agent workflows.
//!
//! The crate provides a provider-neutral model contract and model adapters.

mod model;
mod qwen;

pub use model::{Model, ModelError, ModelRequest, ModelResponse};
pub use qwen::{Qwen, QwenConfig};
