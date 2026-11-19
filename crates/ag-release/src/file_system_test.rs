//! Host filesystem adapter unit tests.

use std::fs;
use std::path::Path;

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

#[test]
fn host_file_system_writes_files_with_parent_directories_and_reads_them_back() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let file = directory.path().join("out/nested/demo.txt");
    let file_system = HostFileSystem;

    // Act
    let written = file_system.write(&file, b"first");
    let replaced = file_system.write(&file, b"second");
    let read = file_system.read(&file);
    let missing = file_system.read(&directory.path().join("missing"));
    let root = file_system.write(Path::new("/"), b"root");

    // Assert
    assert!(written.is_ok());
    assert!(replaced.is_ok());
    assert_eq!(read.expect("read file"), b"second");
    assert!(missing.is_err());
    assert!(root.is_err());
}
