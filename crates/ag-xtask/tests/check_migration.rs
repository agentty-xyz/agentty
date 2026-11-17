//! Public CLI coverage for workspace migration validation.

use std::error::Error;
use std::path::Path;
use std::process::{Command, Output};
use std::{fs, io};

use tempfile::tempdir;

/// Runs Git in `directory` without user or system configuration.
fn git(directory: &Path, arguments: &[&str]) -> io::Result<()> {
    let status = Command::new("git")
        .args(["-c", "user.name=Agentty"])
        .args(["-c", "user.email=agentty@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("git {arguments:?} failed")));
    }

    Ok(())
}

/// Runs `ag-xtask check-migrations` in `directory` against `origin/main`.
fn check_migrations(directory: &Path) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
        .current_dir(directory)
        .arg("check-migrations")
        .env_remove("AGENTTY_MIGRATION_BASE")
        .output()
}

#[test]
fn cli_validates_migrations_and_reports_duplicate_edited_and_unreadable_roots()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempdir()?;
    let root = directory.path();
    let migrations = root.join("crates/example/migrations");
    fs::create_dir_all(&migrations)?;
    fs::write(root.join("crates/README.md"), "Workspace documentation")?;
    fs::write(
        migrations.join("001_first.sql"),
        "CREATE TABLE first (id INTEGER);",
    )?;
    git(root, &["init", "-q"])?;
    let missing_base = check_migrations(root)?;
    git(root, &["add", "."])?;
    git(root, &["commit", "-q", "-m", "Add first migration"])?;
    git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;

    // Act
    fs::write(migrations.join("002_second.sql"), "")?;
    let valid = check_migrations(root)?;
    fs::write(migrations.join("002_duplicate.sql"), "")?;
    let duplicate = check_migrations(root)?;
    fs::remove_file(migrations.join("002_duplicate.sql"))?;
    fs::write(
        migrations.join("001_first.sql"),
        "CREATE TABLE first (id TEXT);",
    )?;
    let edited = check_migrations(root)?;
    fs::remove_dir_all(root.join("crates"))?;
    let missing_root = check_migrations(root)?;

    // Assert
    assert!(!missing_base.status.success());
    assert!(String::from_utf8_lossy(&missing_base.stdout).contains("Cannot find the fork point"));
    assert!(valid.status.success());
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stdout).contains("Duplicate migration prefix"));
    assert!(!edited.status.success());
    assert!(
        String::from_utf8_lossy(&edited.stdout)
            .contains("crates/example/migrations/001_first.sql (modified)")
    );
    assert!(!missing_root.status.success());
    assert!(String::from_utf8_lossy(&missing_root.stdout).contains("Failed to read crates"));

    Ok(())
}
