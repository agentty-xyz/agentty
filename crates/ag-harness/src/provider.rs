//! Provider-specific model adapters.

mod kimi;
mod qwen;

pub use kimi::KimiConfig;
pub(crate) use kimi::POLICY as KIMI_POLICY;
pub(crate) use qwen::POLICY as QWEN_POLICY;
pub use qwen::QwenConfig;
