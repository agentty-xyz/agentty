//! Rust-native TUI end-to-end testing framework.
//!
//! Drives a real TUI binary in a PTY, captures location-aware terminal state
//! with `vt100`, generates VHS tapes for visual screenshots, and provides
//! an assertion API for text, style, color, and region checks.
//!
//! Public items are addressable only through their owning module — there is
//! no crate-root re-export set. Import the specific items each test needs,
//! for example:
//!
//! ```no_run
//! use testty::scenario::Scenario;
//! use testty::session::PtySessionBuilder;
//! ```

pub mod assertion;
pub mod diff;
pub mod feature;
pub mod frame;
pub mod journey;
pub mod locator;
pub mod proof;
pub mod recipe;
pub mod region;
pub mod scenario;
pub mod session;
pub mod snapshot;
pub mod spec;
pub mod step;
pub mod vhs;

pub(crate) mod renderer;
