//! App module router.
//!
//! This parent module intentionally exposes child modules and re-exports app
//! orchestration types and functions.

mod assist;
pub(crate) mod at_mention_task;
mod branch_publish;
mod core;
mod diff_prompt;
mod error;
mod merge_queue;
mod project;
pub(crate) mod prompt_intent;
mod reducer;
mod review;
mod review_request;
mod service;
pub(crate) mod session;
mod session_api;
mod session_creation;
mod session_diff;
mod session_runtime;
pub mod session_state;
pub(crate) mod setting;
mod startup;
mod sync;
pub(crate) mod tab;
mod task;
mod view;

pub use core::{AGENTTY_WT_DIR, App, UpdateStatus, agentty_home};
pub(crate) use core::{AppEvent, AppRuntimeEvent};

pub use error::AppError;
pub use project::ProjectManager;
pub(crate) use review::ReviewCacheEntry;
pub use service::AppServices;
pub use session::{SessionError, SessionManager, SessionState};
pub(crate) use session_diff::SessionDiffUpdate;
pub(crate) use session_runtime::{
    SessionRuntimeAccess, SessionRuntimeCommand, SessionRuntimeHandle,
};
pub use setting::SettingsManager;
pub(crate) use sync::{ProjectSyncPhase, ProjectSyncStatus};
pub use tab::{Tab, TabManager};
pub(crate) use task::TaskService;
pub(crate) use view::AppViewSnapshot;

#[cfg(test)]
#[path = "app_test_support_test.rs"]
pub(crate) mod test_support;
