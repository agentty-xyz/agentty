//! Host filesystem boundary for release inputs and outputs.

use std::io;
use std::path::Path;

/// Filesystem operations used by release commands.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait FileSystem {
    /// Returns whether `path` is a regular file, treating a missing path as
    /// not one.
    ///
    /// # Errors
    /// Returns an error when the path exists but its metadata is unreadable.
    fn is_file(&self, path: &Path) -> io::Result<bool>;
}

/// Host adapter backed by `std::fs`.
pub(crate) struct HostFileSystem;

impl FileSystem for HostFileSystem {
    fn is_file(&self, path: &Path) -> io::Result<bool> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(metadata.is_file()),
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
}

#[cfg(test)]
#[path = "file_system_test.rs"]
mod tests;
