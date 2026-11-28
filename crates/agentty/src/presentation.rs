//! Frontend-neutral interaction state shared by runtime input and UI output.

/// Harness provider API key popup opened from the prompt composer.
pub mod api_key;
/// Frontend-neutral application modes and restorable overlay state.
pub mod app_mode;
/// `ChatGPT` plan sign-in popup opened from the prompt composer.
pub mod chatgpt_sign_in;
/// Snapshot-bound generic execution graph inspection and controls.
pub mod execution_graph;
/// Deterministic wall-clock snapshot shared by one rendered frame.
pub(crate) mod frame_time;
/// Latest proposal and execution snapshot for the graph chat sidebar.
pub(crate) mod graph_chat;
/// Context-sensitive help actions and keybinding projections.
pub mod help_action;
/// Platform sign-in and account projection.
pub mod platform;
/// Explicit project registration and discovery dialogs.
pub mod project_catalog;
/// Prompt composer history, attachment, and suggestion state.
pub mod prompt;
/// Remote-control relay link, pairing, and paired-device projection.
pub mod remote_control;
/// Stable selection projection for grouped review-comment snapshots.
pub(crate) mod review_comment;
/// Session notes panel, its clear confirmation, and the page it restores.
pub mod session_document;
pub(crate) mod setting;
