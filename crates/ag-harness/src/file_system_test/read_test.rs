use std::io;
use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::path::Path;

use tokio::io::AsyncReadExt as _;

use crate::file_system::{FileSystem as _, LocalFileSystem};

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
