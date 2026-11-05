//! Frontend-neutral interaction state shared by runtime input and UI output.

/// Frontend-neutral application modes and restorable overlay state.
pub mod app_mode;
/// `ChatGPT` plan sign-in popup opened from the prompt composer.
pub mod chatgpt_sign_in;
/// Snapshot-bound generic execution graph inspection and controls.
pub mod execution_graph;
/// Deterministic wall-clock snapshot shared by one rendered frame.
pub(crate) mod frame_time;
/// Context-sensitive help actions and keybinding projections.
pub mod help_action;
/// Explicit project registration and discovery dialogs.
pub mod project_catalog;
/// Prompt composer history, attachment, and suggestion state.
pub mod prompt;
/// Stable selection projection for grouped review-comment snapshots.
pub(crate) mod review_comment;
pub(crate) mod setting;
/// Live execution-graph projection and bounded per-session snapshot cache.
pub(crate) mod task_chain_graph;
