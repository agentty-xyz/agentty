//! Manually executed end-to-end checks for live model providers.

type DynError = Box<dyn std::error::Error + Send + Sync>;

#[path = "../support/context_budget.rs"]
mod context_budget_fixture;
mod features;
#[path = "support/greeting.rs"]
mod greeting;
mod image;
mod kimi;
#[path = "../support/model_profile.rs"]
mod model_profile_fixture;
mod muse;
mod muse_read;
mod qwen;
#[path = "support/vision.rs"]
mod vision;
