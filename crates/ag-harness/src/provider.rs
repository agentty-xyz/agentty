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

pub use catalog::{
    ModelConfiguration, ModelConfigurationError, ModelProvider, ModelProviderParseError,
};
pub(crate) use kimi::policy as kimi_policy;
pub use kimi::{KIMI_K2_6, KimiConfig};
pub(crate) use muse::policy as muse_policy;
pub use muse::{MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR, Muse, MuseConfig};
pub(crate) use qwen::policy as qwen_policy;
pub use qwen::{QWEN_PLUS, QwenConfig};
