use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;
use std::time::Duration;

use rustix::fs::{FlockOperation, OFlags};

use crate::file_system::{FILE_OPEN_FLAGS, FileSystem as _, LocalFileSystem};

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

#[tokio::test]
async fn local_file_system_creates_nested_file_atomically() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let file_system = LocalFileSystem;

    // Act
    file_system
        .replace_beneath(
            repository.path(),
            Path::new("nested/output.txt"),
            None,
            b"created\n".to_vec(),
        )
        .await
        .expect("new file should be written");

    // Assert
    assert_eq!(
        std::fs::read(repository.path().join("nested/output.txt"))
            .expect("created file should be readable"),
        b"created\n"
    );
    assert_eq!(
        std::fs::metadata(repository.path().join("nested/output.txt"))
            .expect("created file metadata should be readable")
            .permissions()
            .mode()
            & 0o077,
        0
    );
}

#[tokio::test]
async fn local_file_system_rejects_stale_or_existing_target() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    std::fs::write(repository.path().join("output.txt"), b"current")
        .expect("fixture should be written");
    let file_system = LocalFileSystem;

    // Act
    let stale_error = file_system
        .replace_beneath(
            repository.path(),
            Path::new("output.txt"),
            Some(b"stale".to_vec()),
            b"replacement".to_vec(),
        )
        .await
        .expect_err("stale replacement should fail");
    let create_error = file_system
        .replace_beneath(
            repository.path(),
            Path::new("output.txt"),
            None,
            b"replacement".to_vec(),
        )
        .await
        .expect_err("create over existing file should fail");
    let missing_error = file_system
        .replace_beneath(
            repository.path(),
            Path::new("deleted.txt"),
            Some(b"previous".to_vec()),
            b"replacement".to_vec(),
        )
        .await
        .expect_err("missing update target should be rejected as stale");

    // Assert
    assert_eq!(stale_error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(create_error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(missing_error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        std::fs::read(repository.path().join("output.txt")).expect("original file should remain"),
        b"current"
    );
}

#[test]
fn local_file_system_bounds_update_lock_wait() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let (lock_owner, _) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("first parent descriptor should open");
    let (lock_waiter, _) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("second parent descriptor should open");
    rustix::fs::flock(&lock_owner.descriptor, FlockOperation::LockExclusive)
        .expect("fixture lock should be acquired");

    // Act
    let error =
        LocalFileSystem::acquire_update_lock(&lock_waiter.descriptor, Duration::from_millis(20))
            .expect_err("competing lock should time out");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn local_file_system_propagates_update_lock_errors() {
    // Arrange
    let expected_kind = io::ErrorKind::PermissionDenied;

    // Act
    let error = LocalFileSystem::acquire_update_lock_with(Duration::ZERO, || {
        Err(io::Error::new(expected_kind, "lock failed"))
    })
    .expect_err("lock error should propagate");

    // Assert
    assert_eq!(error.kind(), expected_kind);
}

#[test]
fn local_file_system_keeps_committed_write_when_cleanup_fails() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let target_path = repository.path().join("output.txt");
    let captured_path = repository.path().join(".captured");
    std::fs::write(&target_path, b"model change").expect("target should be written");
    std::fs::write(&captured_path, b"expected").expect("captured file should be written");
    let prepared_file = std::fs::File::open(&target_path).expect("model file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    std::fs::set_permissions(repository.path(), std::fs::Permissions::from_mode(0o550))
        .expect("repository should become read-only");
    let mut remove_captured = false;

    // Act
    let result = LocalFileSystem::validate_exchange(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".captured"),
        b"expected",
        &mut remove_captured,
    );
    std::fs::set_permissions(repository.path(), std::fs::Permissions::from_mode(0o750))
        .expect("repository permissions should be restored");

    // Assert
    result.expect("post-commit cleanup failure should not fail the write");
    assert_eq!(
        std::fs::read(&target_path).expect("target should read"),
        b"model change"
    );
    assert_eq!(
        std::fs::read(&captured_path).expect("recovery file should read"),
        b"expected"
    );
    assert!(!remove_captured);
}

#[tokio::test]
async fn local_file_system_keeps_created_directories_after_failed_write() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let file_system = LocalFileSystem;

    // Act
    let error = file_system
        .replace_beneath(
            repository.path(),
            Path::new("nested/deeper/output.txt"),
            Some(b"missing".to_vec()),
            b"replacement".to_vec(),
        )
        .await
        .expect_err("replacement of missing target should fail");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(repository.path().join("nested/deeper").is_dir());
}

#[test]
fn local_file_system_handles_directory_creation_outcomes() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let (parent, _) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");

    // Act
    LocalFileSystem::create_directory(&parent.descriptor, OsStr::new("created"))
        .expect("missing directory should be created");
    let existing = LocalFileSystem::create_directory(&parent.descriptor, OsStr::new("created"));
    let missing_parent =
        LocalFileSystem::create_directory(&parent.descriptor, OsStr::new("missing/child"))
            .expect_err("missing parent should fail creation");

    // Assert
    existing.expect("existing directory should be accepted");
    assert!(repository.path().join("created").is_dir());
    assert_eq!(missing_parent.kind(), io::ErrorKind::NotFound);
}

#[tokio::test]
async fn local_file_system_rejects_write_symlink_traversal() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let outside = tempfile::tempdir().expect("outside directory should be created");
    let outside_file = outside.path().join("outside.txt");
    std::fs::write(&outside_file, b"outside").expect("outside file should be written");
    symlink(outside.path(), repository.path().join("directory-link"))
        .expect("directory symlink should be created");
    symlink(&outside_file, repository.path().join("file-link"))
        .expect("file symlink should be created");
    let file_system = LocalFileSystem;

    // Act
    let directory_error = file_system
        .replace_beneath(
            repository.path(),
            Path::new("directory-link/outside.txt"),
            Some(b"outside".to_vec()),
            b"changed".to_vec(),
        )
        .await
        .expect_err("directory symlink should not be followed");
    let file_error = file_system
        .replace_beneath(
            repository.path(),
            Path::new("file-link"),
            Some(b"outside".to_vec()),
            b"changed".to_vec(),
        )
        .await
        .expect_err("file symlink should not be followed");

    // Assert
    assert_ne!(directory_error.kind(), io::ErrorKind::NotFound);
    assert_ne!(file_error.kind(), io::ErrorKind::NotFound);
    assert_eq!(
        std::fs::read(outside_file).expect("outside file should remain"),
        b"outside"
    );
}

#[tokio::test]
async fn local_file_system_rejects_invalid_write_paths_and_special_files() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    std::fs::create_dir(repository.path().join("directory"))
        .expect("directory fixture should be created");
    let file_system = LocalFileSystem;

    // Act
    let empty_error = file_system
        .replace_beneath(repository.path(), Path::new(""), None, Vec::new())
        .await
        .expect_err("empty write path should fail");
    let parent_error = file_system
        .replace_beneath(repository.path(), Path::new("../file"), None, Vec::new())
        .await
        .expect_err("parent traversal should fail");
    let directory_error = file_system
        .replace_beneath(
            repository.path(),
            Path::new("directory"),
            Some(Vec::new()),
            Vec::new(),
        )
        .await
        .expect_err("directory target should fail");
    let long_name = "x".repeat(256);
    let long_name_error = file_system
        .replace_beneath(repository.path(), Path::new(&long_name), None, Vec::new())
        .await
        .expect_err("overlong target name should fail");

    // Assert
    assert_eq!(empty_error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(parent_error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(directory_error.kind(), io::ErrorKind::InvalidInput);
    assert_ne!(long_name_error.kind(), io::ErrorKind::NotFound);
}
