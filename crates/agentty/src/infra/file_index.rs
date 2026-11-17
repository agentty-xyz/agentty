//! Filesystem-backed index and fuzzy filtering used by `@` file mentions.

use std::path::Path;

use ignore::WalkBuilder;

use crate::domain::file_entry::FileEntry;

const MAX_DEPTH: usize = 10;

/// Lists files and directories recursively under `root`, respecting
/// `.gitignore`.
///
/// The prompt `@` mention index keeps the full depth-limited result set so
/// file entries are still available in repositories that contain many
/// directories. Directories sort before files; within each group, results are
/// sorted alphabetically by path.
pub fn list_files(root: &Path) -> Vec<FileEntry> {
    let walker = WalkBuilder::new(root)
        .max_depth(Some(MAX_DEPTH))
        .hidden(false)
        .build();

    let mut entries: Vec<FileEntry> = walker
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .is_some_and(|ft| ft.is_file() || ft.is_dir())
        })
        .filter_map(|entry| {
            let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());

            entry.path().strip_prefix(root).ok().and_then(|relative| {
                let path = relative.to_string_lossy().to_string();
                if path.is_empty() {
                    return None;
                }

                Some(FileEntry { is_dir, path })
            })
        })
        .collect();

    entries.sort_by(|first, second| {
        second
            .is_dir
            .cmp(&first.is_dir)
            .then(first.path.cmp(&second.path))
    });

    entries
}

#[cfg(test)]
#[path = "file_index_test.rs"]
mod tests;
