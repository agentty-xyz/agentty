//! Infrastructure adapters for database, filesystem, and system boundaries.
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
/// Process-management utilities for agent subprocess lifecycle.
pub(crate) mod process;
/// Startup project-discovery boundary for home-directory repository scans.
pub mod project_discovery;
/// Tmux process boundary used by app orchestration.
pub mod tmux;
pub mod version;
