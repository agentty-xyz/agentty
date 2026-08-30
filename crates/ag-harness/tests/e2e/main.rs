//! Manually executed end-to-end checks for live model providers.

type DynError = Box<dyn std::error::Error + Send + Sync>;

mod codex;
#[path = "support/greeting.rs"]
mod greeting;
mod kimi;
mod muse;
mod muse_read;
mod qwen;
