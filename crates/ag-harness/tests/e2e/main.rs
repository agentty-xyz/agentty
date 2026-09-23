//! Manually executed end-to-end checks for live model providers.

type DynError = Box<dyn std::error::Error + Send + Sync>;

mod features;
#[path = "support/greeting.rs"]
mod greeting;
mod image;
mod kimi;
mod muse;
mod muse_read;
mod qwen;
#[path = "support/vision.rs"]
mod vision;
