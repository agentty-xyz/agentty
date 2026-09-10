//! App core module router.
//!
//! This parent module intentionally stays router-only and re-exports the
//! `App` facade plus focused child modules for state, startup, draw, and
//! reducer behavior.

mod draw;
mod event;
mod new;
mod state;

pub(crate) use event::{AppEvent, AppRuntimeEvent};
#[cfg(test)]
#[path = "core_test_support_test.rs"]
pub(crate) mod test_support;
pub(crate) use state::SyncReviewRequestTaskResult;
pub use state::{AGENTTY_WT_DIR, App, UpdateStatus};

pub use crate::infra::home::agentty_home;
