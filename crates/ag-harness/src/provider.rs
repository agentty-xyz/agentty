//! Provider-specific model adapters.

mod catalog;
mod codex;
mod kimi;
mod muse;
mod qwen;

pub use catalog::{
    ModelConfiguration, ModelConfigurationError, ModelProvider, ModelProviderParseError,
};
pub(crate) use codex::CodexBackend;
pub use codex::{Codex, CodexConfig};
pub(crate) use kimi::POLICY as KIMI_POLICY;
pub use kimi::{KIMI_K2_6, KimiConfig};
pub(crate) use muse::POLICY as MUSE_POLICY;
pub use muse::{MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR, Muse, MuseConfig};
pub(crate) use qwen::POLICY as QWEN_POLICY;
pub use qwen::{QWEN_PLUS, QwenConfig};
