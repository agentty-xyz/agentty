//! Reusable UI components.

/// Prompt text input and suggestion-dropdown component.
pub mod chat_input;
/// Generic binary-choice confirmation popup.
pub mod confirmation_overlay;
/// Hierarchical changed-file explorer.
pub mod file_explorer;
/// Context-sensitive footer keybindings.
pub mod footer_bar;
/// Scrollable keybinding help popup.
pub mod help_overlay;
/// Informational loading and outcome popup.
pub mod info_overlay;
/// Editable launch-configuration list body.
pub mod launch_configuration_list_editor;
/// Launch-configuration selection popup.
pub mod launch_configuration_overlay;
/// Most-recently-opened project switcher popup.
pub mod project_switcher_overlay;
/// Remote branch name input popup.
pub mod publish_branch_overlay;
/// Calm pulse for queued-action indicators.
pub mod queue_pulse;
/// New-session action selector popup.
pub mod session_creation_overlay;
/// Session transcript, progress, and result rendering.
pub mod session_output;
/// Existing-session stack parent selector.
pub mod stack_append_parent_overlay;
/// Application and session status header.
pub mod status_bar;
/// Top-level navigation tabs.
pub mod tab;
/// Animated terminal loading indicator.
pub mod tachyon_loader;
/// Reusable terminal scrollbar component.
pub mod vertical_scrollbar;
