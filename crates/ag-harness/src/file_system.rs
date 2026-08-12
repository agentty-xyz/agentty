use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use rustix::fs::{FileType, Mode, OFlags};
use tokio::io::AsyncRead;

const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW);
const FILE_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);

/// Asynchronous filesystem boundary used by harness tools.
///
/// The harness uses this boundary for diagnostic path resolution and
/// descriptor-relative opening, keeping repository containment inside the
/// filesystem operation.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// Resolves a path to its canonical absolute representation.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the path cannot be resolved.
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;

    /// Opens a repository-relative file without following symlinks beneath
    /// `root`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when `path` is invalid, traverses a symlink, does
    /// not name a regular file, or cannot be opened for reading.
    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>>;
}

/// Tokio-backed filesystem implementation for local repositories.
pub struct LocalFileSystem;

impl LocalFileSystem {
    fn open_beneath(root: &Path, relative_path: &Path) -> io::Result<std::fs::File> {
        let mut directory =
            rustix::fs::open(root, DIRECTORY_OPEN_FLAGS, Mode::empty()).map_err(io::Error::from)?;
        let components = relative_path
            .components()
            .map(|component| match component {
                Component::Normal(component) => Ok(component),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "read path must be repository-relative",
                )),
            })
            .collect::<io::Result<Vec<&OsStr>>>()?;
        let (file_name, ancestor_components) = components.split_last().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "read path must not be empty")
        })?;

        for component in ancestor_components {
            directory =
                rustix::fs::openat(&directory, *component, DIRECTORY_OPEN_FLAGS, Mode::empty())
                    .map_err(io::Error::from)?;
        }
        let descriptor = rustix::fs::openat(&directory, *file_name, FILE_OPEN_FLAGS, Mode::empty())
            .map_err(io::Error::from)?;
        let metadata = rustix::fs::fstat(&descriptor).map_err(io::Error::from)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read path must name a regular file",
            ));
        }

        Ok(std::fs::File::from(descriptor))
    }
}

#[async_trait]
impl FileSystem for LocalFileSystem {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        tokio::fs::canonicalize(path).await
    }

    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        let root = root.to_path_buf();
        let path = path.to_path_buf();
        let file = tokio::task::spawn_blocking(move || Self::open_beneath(&root, &path))
            .await
            .map_err(io::Error::other)??;

        Ok(Box::new(tokio::fs::File::from_std(file)))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::unix::fs::symlink;

    use tokio::io::AsyncReadExt as _;

    use super::*;

    #[tokio::test]
    async fn local_file_system_canonicalizes_and_opens_file() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let nested_directory = directory.path().join("nested");
        std::fs::create_dir(&nested_directory).expect("nested directory should be created");
        let path = nested_directory.join("input.txt");
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(b"hello"))
            .expect("fixture file should be written");
        let file_system = LocalFileSystem;

        // Act
        let canonical_path = file_system
            .canonicalize(&path)
            .await
            .expect("fixture path should canonicalize");
        let mut file = file_system
            .open_beneath(directory.path(), Path::new("nested/input.txt"))
            .await
            .expect("fixture file should open");
        let mut content = String::new();
        file.read_to_string(&mut content)
            .await
            .expect("fixture file should be readable");

        // Assert
        assert!(canonical_path.is_absolute());
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn local_file_system_reports_missing_paths() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("missing.txt");
        let file_system = LocalFileSystem;

        // Act
        let canonicalize_error = file_system
            .canonicalize(&path)
            .await
            .expect_err("missing path should not canonicalize");
        let open_error = file_system
            .open_beneath(directory.path(), Path::new("missing.txt"))
            .await
            .err()
            .expect("missing path should not open");

        // Assert
        assert_eq!(canonicalize_error.kind(), io::ErrorKind::NotFound);
        assert_eq!(open_error.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn local_file_system_rejects_invalid_relative_paths() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let file_system = LocalFileSystem;

        // Act
        let empty_error = file_system
            .open_beneath(directory.path(), Path::new(""))
            .await
            .err()
            .expect("empty path should fail");
        let parent_error = file_system
            .open_beneath(directory.path(), Path::new("../input.txt"))
            .await
            .err()
            .expect("parent traversal should fail");

        // Assert
        assert_eq!(empty_error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(parent_error.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn local_file_system_rejects_symlink_traversal() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let outside = tempfile::tempdir().expect("outside directory should be created");
        let outside_file = outside.path().join("outside.txt");
        std::fs::File::create(&outside_file)
            .and_then(|mut file| file.write_all(b"outside"))
            .expect("outside file should be written");
        symlink(&outside_file, repository.path().join("file-link"))
            .expect("file symlink should be created");
        symlink(outside.path(), repository.path().join("directory-link"))
            .expect("directory symlink should be created");
        let file_system = LocalFileSystem;

        // Act
        let file_error = file_system
            .open_beneath(repository.path(), Path::new("file-link"))
            .await
            .err()
            .expect("file symlink should not be followed");
        let directory_error = file_system
            .open_beneath(repository.path(), Path::new("directory-link/outside.txt"))
            .await
            .err()
            .expect("directory symlink should not be followed");

        // Assert
        assert_ne!(file_error.kind(), io::ErrorKind::NotFound);
        assert_ne!(directory_error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn local_file_system_opens_nonblocking_and_rejects_non_regular_file() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        std::fs::create_dir(repository.path().join("directory"))
            .expect("directory fixture should be created");

        // Act
        let error = LocalFileSystem::open_beneath(repository.path(), Path::new("directory"))
            .expect_err("directory should not be readable as a regular file");

        // Assert
        assert!(FILE_OPEN_FLAGS.contains(OFlags::NONBLOCK));
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
