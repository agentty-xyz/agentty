//! Quality-enforcement check engine module router.
//!
//! `domain::qe` owns the pure recommendation registry, evaluation engine,
//! and markdown report formatter that back the `/qe:check` slash command.
//! External I/O is routed through the `QeProbe` trait in
//! `crate::infra::qe`, keeping every check deterministic in tests via
//! `MockQeProbe`.

/// Pure execution engine for the recommendation registry.
pub mod engine;
/// Hardcoded recommendation registry plus per-check acceptance closures.
pub mod registry;
/// Markdown rendering for the aggregated `QeReport`.
pub mod report;
/// Pure types describing engine inputs and outputs.
pub mod types;

pub use engine::run;
pub use registry::REGISTRY;
pub use report::render;
pub use types::{QeAcceptance, QeCheckSpec, QeOutcome, QeReport, QeResult};
