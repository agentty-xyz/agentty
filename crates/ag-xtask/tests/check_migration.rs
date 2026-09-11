//! Public CLI coverage for workspace migration validation.

use std::fs;
use std::process::Command;

use tempfile::tempdir;

#[test]
fn cli_validates_migrations_and_reports_duplicate_and_unreadable_roots() {
    // Arrange
    let directory = tempdir().expect("workspace");
    let migrations = directory.path().join("crates/example/migrations");
    fs::create_dir_all(&migrations).expect("migration directory");
    fs::write(
        directory.path().join("crates/README.md"),
        "Workspace documentation",
    )
    .expect("non-crate entry");
    fs::write(migrations.join("001_first.sql"), "").expect("first migration");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .current_dir(directory.path())
            .arg("check-migrations")
            .output()
            .expect("migration checker")
    };

    // Act
    let valid = run();
    fs::write(migrations.join("001_second.sql"), "").expect("duplicate migration");
    let duplicate = run();
    fs::remove_dir_all(directory.path().join("crates")).expect("remove fixture root");
    let missing = run();

    // Assert
    assert!(valid.status.success());
    assert!(!duplicate.status.success());
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stdout).contains("Duplicate migration prefix"));
    assert!(String::from_utf8_lossy(&missing.stdout).contains("Failed to read crates"));
}
