//! File-entry model shared by file indexing, prompt state, and UI layout.

/// A single file or directory entry for `@` mention dropdowns and explorer
/// lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Relative path from the listing root, for example `src/main.rs`.
    pub path: String,
}
