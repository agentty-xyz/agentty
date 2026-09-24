//! Public CLI coverage for execution boundary validation.

use std::path::Path;
use std::process::{Command, Output};
use std::{fs, io};

use tempfile::tempdir;

fn check_execution_boundary(workspace: &Path) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
        .current_dir(workspace)
        .arg("check-execution-boundary")
        .output()
}

fn write_package(workspace: &Path, name: &str, dependencies: &str, source: &str) -> io::Result<()> {
    let package = workspace.join("crates").join(name);
    fs::create_dir_all(package.join("src"))?;
    fs::write(
        package.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
{dependencies}"#
        ),
    )?;

    fs::write(package.join("src/lib.rs"), source)
}

#[test]
fn cli_accepts_the_repository_workspace() {
    // Arrange
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root");

    // Act
    let output = check_execution_boundary(workspace).expect("execution boundary checker");

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn cli_rejects_a_workspace_that_bypasses_the_worker() {
    // Arrange
    let directory = tempdir().expect("workspace");
    fs::write(
        directory.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n",
    )
    .expect("workspace manifest");
    write_package(directory.path(), "ag-runtime", "", "").expect("runtime package");
    write_package(
        directory.path(),
        "agentty",
        "ag-runtime = { path = \"../ag-runtime\" }\n",
        "fn launch() { std::process::Command::new(\"claude\"); }\n",
    )
    .expect("application package");

    // Act
    let output = check_execution_boundary(directory.path()).expect("execution boundary checker");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success());
    assert!(
        stdout.contains("forbidden dependency agentty -> ag-runtime"),
        "{stdout}"
    );
    assert!(
        stdout.contains("agentty/src/lib.rs: launches agent CLI `claude` outside ag-agent"),
        "{stdout}"
    );
}
