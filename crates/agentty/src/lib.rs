pub mod app;
pub mod domain;
pub mod infra;
pub mod ui;

pub mod runtime;

/// Hidden support APIs used by Agentty's integration tests.
#[doc(hidden)]
pub mod test_support;

// Public convenience re-exports.
pub use domain::agent;
pub use infra::{db, version};
pub use ui::icon;
