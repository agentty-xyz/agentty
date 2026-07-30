//! App core module router.
//!
//! This parent module intentionally stays router-only and re-exports the
//! `App` facade plus focused child modules for state, startup, draw, and
//! reducer behavior.

mod draw;
mod events;
mod new;
mod state;

pub(crate) use events::{AppEvent, AppRuntimeEvent};
#[cfg(test)]
pub(crate) use state::AppClients;
pub(crate) use state::SyncReviewRequestTaskResult;
pub use state::{AGENTTY_WT_DIR, App, UpdateStatus};

pub use crate::infra::home::agentty_home;
