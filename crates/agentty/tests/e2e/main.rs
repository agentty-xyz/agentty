//! TUI end-to-end tests for the agentty binary.
//!
//! Uses the `testty` framework to drive the real binary in a PTY and
//! capture terminal frames for semantic assertions.
//!
//! # Running
//!
//! ```sh
//! cargo test -p agentty --test e2e
//! ```

mod common;
mod confirmation;
mod navigation;
mod project;
mod resource;
mod selection;
mod session;
mod setting;
mod update;

#[path = "../support/clock.rs"]
mod clock;
#[path = "../support/database.rs"]
mod test_support;
