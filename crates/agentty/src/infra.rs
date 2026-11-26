//! Infrastructure adapters for database, filesystem, and system boundaries.

/// Workspace-only agent discovery boundary.
pub mod agent_profile;
/// Default-browser boundary for sign-in pages.
pub mod browser;
/// Trusted Cargo cache refreshes for native sandboxed sessions.
pub(crate) mod cargo_cache;
/// Clipboard image capture and persistence boundary for prompt attachments.
pub(crate) mod clipboard_image;
/// Wall-clock boundary used by app, runtime, and session orchestration.
pub mod clock;
pub mod db;
/// Gitignore-aware file indexing and fuzzy path filtering.
pub mod file_index;
/// Filesystem trait boundary used by app orchestration.
pub mod fs;
/// Agentty data-directory path resolution.
pub mod home;
/// Shell-installer receipt interpretation for analytics and updates.
pub(crate) mod install_receipt;
/// Agentty Platform HTTP boundary: device sign-in, account discovery, and
/// instance registration.
pub mod platform;
/// Startup project-discovery boundary for home-directory repository scans.
pub mod project_discovery;
/// Remote-control server and paired-device persistence boundary.
pub(crate) mod remote_control;
/// Host process-accounting boundary.
pub(crate) mod resource;
/// Total provider-call deadline for focused reviews.
pub(crate) mod review_deadline;
/// Terminal clipboard boundary for copying text with OSC 52.
pub(crate) mod terminal_clipboard;
/// Tmux process boundary used by app orchestration.
pub mod tmux;
pub mod version;
