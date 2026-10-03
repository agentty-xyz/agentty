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

mod catalog;
mod kimi;
mod muse;
mod qwen;
#[cfg(test)]
mod test_support;

pub use catalog::{
    ModelConfiguration, ModelConfigurationError, ModelProvider, ModelProviderParseError,
};
pub use kimi::{KIMI_K2_6, KIMI_K3, KimiConfig};
pub use muse::{MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR, Muse, MuseConfig};
pub use qwen::{QWEN_PLUS, QWEN3_8_MAX, QwenConfig};
