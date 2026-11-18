//! Host filesystem boundary shared by workspace checks.

use std::io;
use std::path::{Path, PathBuf};

/// Filesystem operations used by check discovery and validation.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait FileSystem {
    /// Returns the entries directly inside `path`.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;

    /// Returns whether `path` is a directory, treating a missing path as not
    /// one.
    fn is_dir(&self, path: &Path) -> io::Result<bool>;

    /// Returns the UTF-8 contents of `path`.
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
}

/// Host adapter; check decisions remain in each check's policy type.
pub(crate) struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }

    fn is_dir(&self, path: &Path) -> io::Result<bool> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(metadata.is_dir()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
}

#[cfg(test)]
#[path = "file_system_test.rs"]
mod tests;
