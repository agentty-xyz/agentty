//! Host filesystem boundary for release inputs and outputs.

use std::path::Path;
use std::{fs, io};

/// Filesystem operations used by release commands.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait FileSystem {
    /// Returns whether `path` is a regular file, treating a missing path as
    /// not one.
    ///
    /// # Errors
    /// Returns an error when the path exists but its metadata is unreadable.
    fn is_file(&self, path: &Path) -> io::Result<bool>;

    /// Returns the contents of the file at `path`.
    ///
    /// # Errors
    /// Returns an error when the file is missing or unreadable.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Writes `contents` to the file at `path`, creating its parent
    /// directories and replacing an existing file.
    ///
    /// # Errors
    /// Returns an error when a directory or the file cannot be written.
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()>;
}

/// Host adapter backed by `std::fs`.
pub(crate) struct HostFileSystem;

impl FileSystem for HostFileSystem {
    fn is_file(&self, path: &Path) -> io::Result<bool> {
        match fs::metadata(path) {
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

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, contents)
    }
}

#[cfg(test)]
#[path = "file_system_test.rs"]
mod tests;
