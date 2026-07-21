//! Frontend-neutral interaction state shared by runtime input and UI output.

/// Frontend-neutral application modes and restorable overlay state.
pub mod app_mode;
/// Deterministic wall-clock snapshot shared by one rendered frame.
pub(crate) mod frame_time;
/// Context-sensitive help actions and keybinding projections.
pub mod help_action;
/// Prompt composer history, attachment, and suggestion state.
pub mod prompt;
/// Stable selection projection for grouped review-comment snapshots.
pub(crate) mod review_comment;
pub(crate) mod settings;
