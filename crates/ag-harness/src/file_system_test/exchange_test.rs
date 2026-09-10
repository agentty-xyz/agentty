use std::ffi::OsStr;
use std::io;
use std::path::Path;

use super::support::FailingReader;
use crate::file_system::LocalFileSystem;

#[test]
fn local_file_system_does_not_overwrite_target_changed_at_install() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    std::fs::write(repository.path().join("output.txt"), b"newer")
        .expect("newer target should be written");
    std::fs::write(repository.path().join(".prepared"), b"model change")
        .expect("prepared file should be written");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    let prepared_file = std::fs::OpenOptions::new()
        .write(true)
        .open(repository.path().join(".prepared"))
        .expect("prepared file should open");
    let mut remove_prepared = true;

    // Act
    let error = LocalFileSystem::install_update(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".prepared"),
        b"stale",
        &mut remove_prepared,
    )
    .expect_err("changed target should reject installation");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(remove_prepared);
    assert_eq!(
        std::fs::read(repository.path().join("output.txt")).expect("newer target should remain"),
        b"newer"
    );
}

#[test]
fn local_file_system_rolls_back_stale_atomic_exchange() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    std::fs::write(repository.path().join("output.txt"), b"model change")
        .expect("exchanged model file should be written");
    std::fs::write(repository.path().join(".prepared"), b"newer")
        .expect("captured newer file should be written");
    let prepared_file =
        std::fs::File::open(repository.path().join("output.txt")).expect("model file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    let mut remove_prepared = false;

    // Act
    let error = LocalFileSystem::validate_exchange(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".prepared"),
        b"expected",
        &mut remove_prepared,
    )
    .expect_err("stale exchange should be rolled back");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(remove_prepared);
    assert_eq!(
        std::fs::read(repository.path().join("output.txt"))
            .expect("newer target should be restored"),
        b"newer"
    );
    assert_eq!(
        std::fs::read(repository.path().join(".prepared"))
            .expect("model file should remain recoverable"),
        b"model change"
    );
}

#[test]
fn local_file_system_rejects_replaced_target_after_exchange() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let target_path = repository.path().join("output.txt");
    let captured_path = repository.path().join(".captured");
    let prepared_path = repository.path().join(".opened-prepared");
    std::fs::write(&target_path, b"concurrent").expect("concurrent target should be written");
    std::fs::write(&captured_path, b"expected").expect("captured file should be written");
    std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
    let prepared_file = std::fs::File::open(prepared_path).expect("prepared file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    let mut remove_captured = false;

    // Act
    let error = LocalFileSystem::validate_exchange(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".captured"),
        b"expected",
        &mut remove_captured,
    )
    .expect_err("replaced target should be rejected");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!remove_captured);
    assert_eq!(
        std::fs::read(target_path).expect("concurrent target should remain"),
        b"concurrent"
    );
    assert_eq!(
        std::fs::read(captured_path).expect("captured target should remain recoverable"),
        b"expected"
    );
}

#[test]
fn local_file_system_reports_target_identity_lookup_errors() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let expected_path = repository.path().join("expected.txt");
    std::fs::write(&expected_path, b"expected").expect("expected file should be written");
    std::fs::write(repository.path().join("blocked"), b"file")
        .expect("blocking file should be written");
    let expected_file = std::fs::File::open(expected_path).expect("expected file should open");
    let (parent, _) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");

    // Act
    let missing_matches = LocalFileSystem::path_matches_file(
        &parent.descriptor,
        OsStr::new("missing.txt"),
        &expected_file,
    )
    .expect("missing target should compare");
    let traversal_error = LocalFileSystem::path_matches_file(
        &parent.descriptor,
        OsStr::new("blocked/child"),
        &expected_file,
    )
    .expect_err("non-directory traversal should fail");

    // Assert
    assert!(!missing_matches);
    assert_eq!(traversal_error.kind(), io::ErrorKind::NotADirectory);
}

#[test]
fn local_file_system_restores_concurrent_target_during_rollback() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let target_path = repository.path().join("output.txt");
    let captured_path = repository.path().join(".captured");
    let prepared_path = repository.path().join(".opened-prepared");
    std::fs::write(&target_path, b"concurrent").expect("concurrent target should be written");
    std::fs::write(&captured_path, b"newer").expect("captured file should be written");
    std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
    let prepared_file = std::fs::File::open(prepared_path).expect("prepared file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    let mut remove_captured = false;

    // Act
    let error = LocalFileSystem::validate_exchange(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".captured"),
        b"expected",
        &mut remove_captured,
    )
    .expect_err("concurrent rollback target should be rejected");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!remove_captured);
    assert_eq!(
        std::fs::read(target_path).expect("concurrent target should be restored"),
        b"concurrent"
    );
    assert_eq!(
        std::fs::read(captured_path).expect("captured target should remain recoverable"),
        b"newer"
    );
}

#[test]
fn local_file_system_preserves_recovery_when_exchange_path_disappears() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let target_path = repository.path().join("output.txt");
    let prepared_path = repository.path().join(".prepared");
    std::fs::write(&target_path, b"model change").expect("model file should be written");
    std::fs::write(&prepared_path, b"newer").expect("captured file should be written");
    let prepared_file = std::fs::File::open(&target_path).expect("model file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    std::fs::remove_file(&target_path).expect("model path should disappear");
    let mut remove_prepared = false;

    // Act
    let error = LocalFileSystem::validate_exchange(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".prepared"),
        b"expected",
        &mut remove_prepared,
    )
    .expect_err("stale exchange should be rejected");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!remove_prepared);
    assert!(!target_path.exists());
    assert_eq!(
        std::fs::read(prepared_path).expect("captured target should remain recoverable"),
        b"newer"
    );
}

#[test]
fn local_file_system_reports_failed_exchange_rollback() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let prepared_path = repository.path().join(".opened-prepared");
    std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
    let prepared_file = std::fs::File::open(prepared_path).expect("prepared file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    let mut remove_prepared = false;

    // Act
    let error = LocalFileSystem::roll_back_exchange(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".missing-prepared"),
        &mut remove_prepared,
    )
    .expect_err("missing rollback paths should fail");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!remove_prepared);
}

#[test]
fn local_file_system_preserves_non_missing_exchange_errors() {
    // Arrange
    let permission_error = io::Error::new(io::ErrorKind::PermissionDenied, "denied");

    // Act
    let mapped = LocalFileSystem::map_exchange_error(permission_error);

    // Assert
    assert_eq!(mapped.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(mapped.to_string(), "denied");
}

#[test]
fn local_file_system_preserves_target_when_exchange_fails() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    std::fs::write(repository.path().join("output.txt"), b"expected")
        .expect("target should be written");
    let prepared_path = repository.path().join(".opened-prepared");
    std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
    let prepared_file = std::fs::OpenOptions::new()
        .write(true)
        .open(prepared_path)
        .expect("prepared file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    let mut remove_prepared = true;

    // Act
    let error = LocalFileSystem::install_update(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".missing-prepared"),
        b"expected",
        &mut remove_prepared,
    )
    .expect_err("missing prepared file should fail exchange");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(remove_prepared);
    assert_eq!(
        std::fs::read(repository.path().join("output.txt"))
            .expect("target should remain available"),
        b"expected"
    );
}

#[test]
fn target_comparison_reads_at_most_one_byte_past_expected() {
    // Arrange
    let expected = b"expected";
    let mut exact = io::Cursor::new(expected);
    let mut shorter = io::Cursor::new(b"expect".as_slice());
    let mut different = io::Cursor::new(b"expEcted".as_slice());
    let mut longer = io::Cursor::new(b"expected and arbitrarily more data".as_slice());
    let mut expected_failure = FailingReader;
    let mut extra_failure = FailingReader;

    // Act
    let exact_matches =
        LocalFileSystem::target_matches(&mut exact, expected).expect("exact target should compare");
    let shorter_matches = LocalFileSystem::target_matches(&mut shorter, expected)
        .expect("short target should compare");
    let different_matches = LocalFileSystem::target_matches(&mut different, expected)
        .expect("different target should compare");
    let longer_matches =
        LocalFileSystem::target_matches(&mut longer, expected).expect("long target should compare");
    let expected_error = LocalFileSystem::target_matches(&mut expected_failure, expected)
        .expect_err("expected-content read error should propagate");
    let extra_error = LocalFileSystem::target_matches(&mut extra_failure, b"")
        .expect_err("extra-byte read error should propagate");

    // Assert
    assert!(exact_matches);
    assert!(!shorter_matches);
    assert!(!different_matches);
    assert!(!longer_matches);
    assert_eq!(longer.position(), (expected.len() + 1) as u64);
    assert_eq!(expected_error.kind(), io::ErrorKind::Other);
    assert_eq!(extra_error.kind(), io::ErrorKind::Other);
}
