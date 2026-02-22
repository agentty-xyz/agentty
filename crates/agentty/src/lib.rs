pub mod app;
pub mod domain;
pub mod infra;
pub mod ui;

pub mod runtime;

// Re-exports for backward compatibility and convenience
pub use domain::agent;
pub use infra::{db, git, lock, version};
pub use ui::icon;
