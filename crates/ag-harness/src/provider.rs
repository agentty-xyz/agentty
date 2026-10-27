//! Built-in model clients.
//!
//! Each provider reads its API key from the environment:
//!
//! ```no_run
//! use ag_harness::provider::{MUSE_SPARK_1_3, Muse};
//!
//! let model = Muse::from_env(MUSE_SPARK_1_3)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [`ModelConfiguration`] selects a provider and model by name at runtime.
//! [`ModelProvider::OpenAiCompatible`] serves any model of a local or
//! self-hosted OpenAI-compatible server, by default Ollama's
//! `http://127.0.0.1:11434/v1`, with an optional API key and a context
//! profile declared through [`ModelProvider::environment_profile`].

mod catalog;
mod kimi;
mod muse;
mod openai_compatible;
mod qwen;
#[cfg(test)]
mod test_support;

pub use catalog::{
    ModelConfiguration, ModelConfigurationError, ModelProvider, ModelProviderParseError,
};
pub use kimi::{KIMI_K2_6, KIMI_K3, KimiConfig};
pub use muse::{MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR, Muse, MuseConfig};
pub use openai_compatible::{
    DEFAULT_CONTEXT_WINDOW_TOKENS, DEFAULT_MAX_OUTPUT_TOKENS, OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV,
    OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV, OpenAiCompatibleConfig,
};
pub use qwen::{QWEN_PLUS, QWEN3_8_MAX, QwenConfig};
