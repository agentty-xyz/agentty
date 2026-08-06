//! Provider-specific model adapters.

mod kimi;
mod qwen;

pub use kimi::{Kimi, KimiConfig};
pub use qwen::{Qwen, QwenConfig};
