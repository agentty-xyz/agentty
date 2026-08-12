//! App module router.
//!
//! This parent module intentionally exposes child modules and re-exports app
//! orchestration types and functions.

mod assist;
pub(crate) mod at_mention_task;
mod branch_publish;
mod core;
mod error;
mod merge_queue;
mod orchestration;
mod project;
pub(crate) mod prompt_intent;
mod reducer;
mod review;
mod review_request;
mod service;
pub(crate) mod session;
mod session_api;
mod session_runtime;
pub mod session_state;
pub(crate) mod setting;
mod startup;
mod sync;
pub(crate) mod sync_message;
pub(crate) mod tab;
mod task;
mod view;

#[cfg(test)]
pub(crate) use core::AppClients;
pub use core::{AGENTTY_WT_DIR, App, UpdateStatus, agentty_home};
pub(crate) use core::{AppEvent, AppRuntimeEvent};

pub use error::AppError;
pub(crate) use orchestration::{
    OrchestrationApprovalOutcome, OrchestrationCoordinator, OrchestrationSchedule,
};
pub use project::ProjectManager;
#[cfg(test)]
pub(crate) use review::review_loading_message;
pub(crate) use review::{ReviewCacheEntry, diff_content_hash};
#[cfg(test)]
pub(crate) use service::AppServiceDeps;
pub use service::AppServices;
pub use session::{SessionError, SessionManager, SessionState};
#[cfg(test)]
pub(crate) use session::{SyncMainOutcome, SyncSessionStartError};
pub(crate) use session_runtime::{
    SessionRuntimeAccess, SessionRuntimeCommand, SessionRuntimeHandle,
};
pub use setting::SettingsManager;
#[cfg(test)]
pub(crate) use sync::MockSyncMainRunner;
pub use tab::{Tab, TabManager};
pub(crate) use task::TaskService;
pub(crate) use view::AppViewSnapshot;
