//! `ag-release plan` command-line contract.

use std::error::Error;
use std::path::Path;
use std::process::{Command, Output};
use std::{fs, io};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

/// Root manifest of a one-app demo workspace.
const DEMO_MANIFEST: &str = r#"[workspace]
members = ["demo-app"]
resolver = "2"

[workspace.metadata.release]
include = ["README.md"]
installers = ["shell"]

[workspace.metadata.release.targets]
x86_64-unknown-linux-gnu = "ubuntu-22.04"

[workspace.metadata.release.apps.demo-app]
"#;

/// Manifest of the demo workspace's only member.
const DEMO_APP_MANIFEST: &str = r#"[package]
name = "demo-app"
version = "0.1.0"
edition = "2024"
repository = "https://github.com/demo-org/demo-app"
"#;

/// Creates the demo workspace in a temporary directory.
fn demo_workspace() -> io::Result<TempDir> {
    let directory = tempdir()?;
    let root = directory.path();
    fs::write(root.join("Cargo.toml"), DEMO_MANIFEST)?;
    fs::write(root.join("README.md"), "# demo-app\n")?;
    fs::create_dir_all(root.join("demo-app/src"))?;
    fs::write(root.join("demo-app/Cargo.toml"), DEMO_APP_MANIFEST)?;
    fs::write(root.join("demo-app/src/main.rs"), "fn main() {}\n")?;

    Ok(directory)
}

/// Runs `ag-release` in `current_dir` without `CARGO`, as when invoked outside
/// Cargo, so it falls back to `cargo` on `PATH`.
fn ag_release(current_dir: &Path, args: &[&str]) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_ag-release"))
        .args(args)
        .current_dir(current_dir)
        .env_remove("CARGO")
        .output()
}

#[test]
fn plan_prints_json_on_stdout_and_fails_with_an_error_on_stderr() -> Result<(), Box<dyn Error>> {
    // Arrange
    let workspace = demo_workspace()?;

    // Act
    let planned = ag_release(workspace.path(), &["plan", "--tag", "v0.1.0"])?;
    let mismatched = ag_release(workspace.path(), &["plan", "--tag", "v0.2.0"])?;

    // Assert
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let plan: Value = serde_json::from_slice(&planned.stdout)?;
    assert_eq!(plan["tag"], "v0.1.0");
    assert_eq!(plan["apps"][0]["binaries"], json!(["demo-app"]));
    assert_eq!(plan["apps"][0]["shell_installer"], "demo-app-installer.sh");
    assert_eq!(
        plan["matrix"]["include"][0]["target"],
        "x86_64-unknown-linux-gnu"
    );
    assert!(!mismatched.status.success());
    assert_eq!(String::from_utf8_lossy(&mismatched.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&mismatched.stderr),
        "error: release tag `v0.2.0` names version 0.2.0, but the apps have version 0.1.0\n"
    );

    Ok(())
}

#[test]
fn plan_accepts_this_repository_configuration() -> io::Result<()> {
    // Act
    let planned = ag_release(Path::new(env!("CARGO_MANIFEST_DIR")), &["plan"])?;

    // Assert
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );

    Ok(())
}
