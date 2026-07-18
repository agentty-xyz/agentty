//! Frontend-neutral interaction state shared by runtime input and UI output.

/// Frontend-neutral application modes and restorable overlay state.
pub mod app_mode;
/// Context-sensitive help actions and keybinding projections.
pub mod help_action;
/// Prompt composer history, attachment, and suggestion state.
pub mod prompt;
