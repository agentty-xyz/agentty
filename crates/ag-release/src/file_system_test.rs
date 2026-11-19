//! Host filesystem adapter unit tests.

use std::fs;

use tempfile::tempdir;

use crate::file_system::{FileSystem, HostFileSystem};

#[test]
fn host_file_system_distinguishes_files_from_other_paths() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let file = directory.path().join("README.md");
    fs::write(&file, "readme\n").expect("file");
    let file_system = HostFileSystem;

    // Act
    let is_file = file_system.is_file(&file);
    let is_directory = file_system.is_file(directory.path());
    let is_missing = file_system.is_file(&directory.path().join("missing"));
    let below_file = file_system.is_file(&file.join("child"));
    let invalid_name = file_system.is_file(&directory.path().join("x".repeat(300)));

    // Assert
    assert!(is_file.expect("file"));
    assert!(!is_directory.expect("directory"));
    assert!(!is_missing.expect("missing"));
    assert!(!below_file.expect("below file"));
    assert!(invalid_name.is_err());
}
