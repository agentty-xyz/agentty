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
//! [`ModelProvider::OpenAi`] serves `OpenAI` models through the stateless,
//! streamed Responses API with `OPENAI_API_KEY`.
//! [`ModelProvider::OpenAiCompatible`] serves any model of a local or
//! self-hosted OpenAI-compatible server, by default Ollama's
//! `http://127.0.0.1:11434/v1`, with an optional API key and a context
//! profile declared through [`ModelProvider::declared_profile`] or sized to
//! the window the server runs through [`ModelProvider::running_profile`].
//! [`ModelProvider::local_server`] lists that server's models through
//! [`LocalServer`]. [`ModelProvider::cost_tier`] declares each cataloged
//! model's [`CostTier`], so hosts can route simple tasks to cheap models.

mod catalog;
mod kimi;
mod muse;
mod openai;
mod openai_compatible;
mod qwen;
#[cfg(test)]
mod test_support;

pub use ag_router::{LocalInventory, LocalModel, LocalServer, LocalServerError, LocalServerKind};
pub use catalog::{
    CostTier, ModelConfiguration, ModelConfigurationError, ModelProvider, ModelProviderParseError,
};
pub use kimi::{KIMI_K2_6, KIMI_K3, KimiConfig};
pub use muse::{MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR, Muse, MuseConfig};
pub use openai::{
    GPT_5_3_CODEX, GPT_5_6_TERRA, GPT_6_1_SOL, GPT_6_ASTRA, GPT_6_LUNA, OpenAiConfig,
};
pub use openai_compatible::{
    DEFAULT_CONTEXT_WINDOW_TOKENS, DEFAULT_MAX_OUTPUT_TOKENS, OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV,
    OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV, OpenAiCompatibleConfig,
};
pub use qwen::{QWEN_PLUS, QWEN3_7_FLASH, QWEN3_8_FLASH, QWEN3_8_MAX, QwenConfig};
