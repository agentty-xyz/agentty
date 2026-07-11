//! File-entry model shared by file indexing, prompt state, and UI layout.

use std::path::PathBuf;

/// Selects the directory backing an active `@`-mention lookup.
#[must_use]
pub fn at_mention_lookup_root(
    project_working_dir: PathBuf,
    session_folder: Option<PathBuf>,
    has_session_folder: bool,
) -> PathBuf {
    if has_session_folder && let Some(session_folder) = session_folder {
        return session_folder;
    }

    project_working_dir
}

/// A single file or directory entry for `@` mention dropdowns and explorer
/// lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Relative path from the listing root, for example `src/main.rs`.
    pub path: String,
}
