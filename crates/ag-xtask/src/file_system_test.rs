//! Host filesystem adapter unit tests.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, io};

use tempfile::tempdir;

use crate::file_system::{FileSystem, RealFileSystem};

#[test]
fn real_file_system_lists_reads_and_distinguishes_directories() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let file = directory.path().join("app.rs");
    fs::write(&file, "mod child;\n").expect("source file");
    let file_system = RealFileSystem;

    // Act
    let entries = file_system.read_dir(directory.path());
    let missing_entries = file_system.read_dir(&directory.path().join("missing"));
    let text = file_system.read_to_string(&file);
    let is_directory = file_system.is_dir(directory.path());
    let is_file = file_system.is_dir(&file);
    let is_missing = file_system.is_dir(&directory.path().join("missing"));
    let below_file = file_system.is_dir(&file.join("child"));
    let invalid_metadata = file_system.is_dir(&directory.path().join("x".repeat(300)));

    // Assert
    assert_eq!(entries.expect("entries"), vec![file]);
    assert!(missing_entries.is_err());
    assert_eq!(text.expect("text"), "mod child;\n");
    assert!(is_directory.expect("directory"));
    assert!(!is_file.expect("file"));
    assert!(!is_missing.expect("missing"));
    assert!(!below_file.expect("below file"));
    assert!(invalid_metadata.is_err());
}

#[cfg(unix)]
#[test]
fn real_file_system_reports_metadata_permission_failures() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let locked = directory.path().join("locked");
    fs::create_dir(&locked).expect("locked directory");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("lock directory");

    // Act
    let result = RealFileSystem.is_dir(&locked.join("child"));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("unlock directory");

    // Assert
    assert_eq!(
        result.expect_err("permission failure").kind(),
        io::ErrorKind::PermissionDenied
    );
}
