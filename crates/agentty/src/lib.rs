//! Agentty application library.
//!
//! The binary composition root uses these layered modules to run the terminal
//! application, while integration tests use the same surface to exercise
//! workflows without duplicating production wiring. Reusable provider, Git,
//! protocol, and terminal-text APIs live in their dedicated workspace crates.

pub mod app;
pub mod domain;
pub mod infra;
pub mod presentation;
/// Terminal rendering components, pages, and formatting helpers.
pub mod ui;

pub mod runtime;

/// Hidden support APIs used by Agentty's integration tests.
#[doc(hidden)]
pub mod test_support;

// Public convenience re-exports.
pub use infra::db;
