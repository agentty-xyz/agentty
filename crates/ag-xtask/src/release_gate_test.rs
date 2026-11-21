//! Release-check decision unit tests.

use std::path::PathBuf;

use super::{Decision, workspace_version};

fn paths(paths: &[&str]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

#[test]
fn a_moved_workspace_version_runs_the_checks() {
    // Arrange
    let files = paths(&["Cargo.toml", "CHANGELOG.md"]);
    // Act
    let decision = Decision::new("0.19.7", "0.19.8", &files);
    // Assert
    assert_eq!(
        decision,
        Decision::VersionChanged {
            from: "0.19.7".to_string(),
            to: "0.19.8".to_string(),
        }
    );
    assert!(decision.runs());
    assert_eq!(
        decision.reason(),
        "Release checks run: the workspace version moves from 0.19.7 to 0.19.8"
    );
}

#[test]
fn a_changed_check_input_runs_the_checks() {
    // Arrange
    let files = paths(&[
        "rust-toolchain.toml",
        "crates/core/src/lib.rs",
        ".pre-commit-config.yaml",
    ]);
    // Act
    let decision = Decision::new("0.19.7", "0.19.7", &files);
    // Assert
    assert_eq!(
        decision,
        Decision::InputsChanged(paths(&["rust-toolchain.toml", ".pre-commit-config.yaml"]))
    );
    assert!(decision.runs());
    assert_eq!(
        decision.reason(),
        "Release checks run: their inputs changed: rust-toolchain.toml, .pre-commit-config.yaml"
    );
}

#[test]
fn other_changes_skip_the_checks() {
    // Arrange
    let files = paths(&["crates/core/src/lib.rs", "nested/rust-toolchain.toml"]);
    // Act
    let decision = Decision::new("0.19.7", "0.19.7", &files);
    // Assert
    assert_eq!(decision, Decision::Unchanged);
    assert!(!decision.runs());
    assert!(decision.reason().starts_with("Release checks skipped"));
}

#[test]
fn workspace_version_reads_the_workspace_package_table() {
    // Arrange
    let manifest =
        "[workspace]\nmembers = []\n\n[workspace.package] # shared\nversion = \"1.2.3\"\n";
    // Act
    let version = workspace_version(manifest);
    // Assert
    assert_eq!(version, Ok("1.2.3".to_string()));
}

#[test]
fn workspace_version_rejects_a_manifest_without_a_version() {
    // Arrange
    let manifest = "[workspace.package]\nedition = \"2024\"\n\n[package]\nversion = \"1.0.0\"\n";
    // Act
    let version = workspace_version(manifest);
    // Assert
    assert_eq!(
        version,
        Err("The workspace Cargo.toml has no [workspace.package].version".to_string())
    );
}

#[test]
fn workspace_version_rejects_invalid_toml() {
    // Arrange
    let manifest = "[workspace.package\n";
    // Act
    let version = workspace_version(manifest);
    // Assert
    assert!(version.is_err_and(|error| error.starts_with("Could not parse")));
}
