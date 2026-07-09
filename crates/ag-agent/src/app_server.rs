//! Shared app-server module router.
//!
//! This parent module keeps transport internals private while re-exporting the
//! provider-neutral app-server contract types consumed from the crate root.

mod contract;
mod error;
pub(crate) mod prompt;
mod registry;
mod retry;

pub(crate) use contract::BorrowedAppServerFuture;
#[cfg(any(test, feature = "test-utils"))]
pub use contract::MockAppServerClient;
pub use contract::{
    AppServerClient, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    AppServerTurnResponse,
};
pub use error::AppServerError;
pub(crate) use registry::AppServerSessionRegistry;
pub(crate) use retry::{RuntimeInspector, run_turn_with_restart_retry};
