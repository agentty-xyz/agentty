//! Presentation contracts shared by app orchestration, runtime input, and UI
//! rendering.

pub mod app_mode;
pub mod help_action;
pub mod icon;
pub mod prompt;
/// Shared theme-aware presentation styling and status-display helpers.
pub mod style;
/// User-visible sync-status message formatting shared by app and UI layers.
pub mod sync_message;
pub mod table_state;
