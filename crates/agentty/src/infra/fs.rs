//! Filesystem boundary used by app orchestration workflows.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

/// Boxed async result used by [`FsClient`] trait methods.
pub type FsFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Typed error returned by filesystem infrastructure operations.
///
/// Wraps I/O failures so callers can distinguish filesystem errors without
/// parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    /// A filesystem or file I/O operation failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Async filesystem boundary used by app-layer workflows.
///
/// Production uses [`RealFsClient`], while tests can inject
/// `MockFsClient` to avoid mutating the real filesystem.
#[cfg_attr(test, mockall::automock)]
pub trait FsClient: Send + Sync {
    /// Reclaims registered stale agent archives in immediate child worktrees
    /// of the trusted, repository-external managed-worktree `root`.
    ///
    /// # Errors
    /// Returns an error when recovery cannot inspect or remove an archive.
    fn cleanup_agent_artifacts(&self, root: PathBuf) -> FsFuture<Result<(), FsError>>;

    /// Recursively creates `path` and its missing parents.
    ///
    /// # Errors
    /// Returns an error when filesystem creation fails.
    fn create_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), FsError>>;

    /// Recursively removes `path` and its contents.
    ///
    /// # Errors
    /// Returns an error when filesystem removal fails.
    fn remove_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), FsError>>;

    /// Removes one empty directory at `path`.
    ///
    /// Fails with [`std::io::ErrorKind::DirectoryNotEmpty`] when the directory
    /// still contains entries, allowing callers to safely prune shared
    /// directories only when no sibling files remain.
    ///
    /// # Errors
    /// Returns an error when filesystem removal fails for any reason,
    /// including the directory still being non-empty.
    fn remove_dir(&self, path: PathBuf) -> FsFuture<Result<(), FsError>>;

    /// Reads one file into bytes without blocking the async runtime.
    ///
    /// # Errors
    /// Returns an error when file read fails.
    fn read_file(&self, path: PathBuf) -> FsFuture<Result<Vec<u8>, FsError>>;

    /// Writes one byte buffer to `path`, replacing any existing file.
    ///
    /// # Errors
    /// Returns an error when file creation or write fails.
    fn write_file(&self, path: PathBuf, contents: Vec<u8>) -> FsFuture<Result<(), FsError>>;

    /// Removes one file from disk.
    ///
    /// Missing files are treated as a successful no-op.
    ///
    /// # Errors
    /// Returns an error when filesystem removal fails for any reason other
    /// than the file already being absent.
    fn remove_file(&self, path: PathBuf) -> FsFuture<Result<(), FsError>>;

    /// Resolves `path` to its canonical absolute filesystem location.
    ///
    /// # Errors
    /// Returns an error when path resolution fails.
    fn canonicalize(&self, path: PathBuf) -> FsFuture<Result<PathBuf, FsError>>;

    /// Returns whether `path` currently resolves to an existing filesystem
    /// entry of any kind.
    fn exists(&self, path: PathBuf) -> bool;

    /// Returns whether `path` currently resolves to an existing directory.
    fn is_dir(&self, path: PathBuf) -> bool;

    /// Returns whether `path` currently resolves to an existing regular file.
    fn is_file(&self, path: PathBuf) -> bool;
}

/// Production [`FsClient`] implementation backed by real filesystem calls.
pub struct RealFsClient;

impl FsClient for RealFsClient {
    fn cleanup_agent_artifacts(&self, root: PathBuf) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let entries = match std::fs::read_dir(root) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(FsError::from(error)),
                };
                for entry in entries {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        ag_agent::cleanup_session_worktree_artifacts(&entry.path())
                            .map_err(std::io::Error::other)?;
                    }
                }

                Ok(())
            })
            .await
            .map_err(std::io::Error::other)?
        })
    }

    fn create_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move { tokio::fs::create_dir_all(path).await.map_err(FsError::from) })
    }

    fn remove_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move { tokio::fs::remove_dir_all(path).await.map_err(FsError::from) })
    }

    fn remove_dir(&self, path: PathBuf) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move { tokio::fs::remove_dir(path).await.map_err(FsError::from) })
    }

    fn read_file(&self, path: PathBuf) -> FsFuture<Result<Vec<u8>, FsError>> {
        Box::pin(async move { tokio::fs::read(path).await.map_err(FsError::from) })
    }

    fn write_file(&self, path: PathBuf, contents: Vec<u8>) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move {
            tokio::fs::write(path, contents)
                .await
                .map_err(FsError::from)
        })
    }

    fn remove_file(&self, path: PathBuf) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move {
            match tokio::fs::remove_file(path).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(FsError::from(error)),
            }
        })
    }

    fn canonicalize(&self, path: PathBuf) -> FsFuture<Result<PathBuf, FsError>> {
        Box::pin(async move { tokio::fs::canonicalize(path).await.map_err(FsError::from) })
    }

    fn exists(&self, path: PathBuf) -> bool {
        path.exists()
    }

    fn is_dir(&self, path: PathBuf) -> bool {
        path.is_dir()
    }

    fn is_file(&self, path: PathBuf) -> bool {
        path.is_file()
    }
}

#[cfg(test)]
#[path = "fs_test.rs"]
mod tests;
