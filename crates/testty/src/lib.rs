//! Rust-native TUI end-to-end testing framework.
//!
//! Drives a real TUI binary in a PTY, captures location-aware terminal state
//! with `vt100`, generates VHS tapes for visual screenshots, and provides
//! an assertion API for text, style, color, and region checks.
//!
//! Most users only need the curated [`prelude`]:
//!
//! ```no_run
//! use testty::prelude::*;
//! ```

pub mod assertion;
pub mod diff;
pub mod feature;
pub mod frame;
pub mod journey;
pub mod locator;
pub mod prelude;
pub mod proof;
pub mod recipe;
pub mod region;
pub mod scenario;
pub mod session;
pub mod snapshot;
pub mod step;
pub mod vhs;

pub(crate) mod renderer;
