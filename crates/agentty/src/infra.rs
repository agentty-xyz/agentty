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
/// Process-management utilities for agent subprocess lifecycle.
pub(crate) mod process;
/// Startup project-discovery boundary for home-directory repository scans.
pub mod project_discovery;
/// In-memory cache for review-request inline comment threads polled by the
/// background sync task and read by the review-comments preview page.
pub mod review_comment_cache;
/// Tmux process boundary used by app orchestration.
pub mod tmux;
pub mod version;
