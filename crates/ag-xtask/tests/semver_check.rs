//! Public published-crate semver check CLI contracts with an injected Cargo.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

use tempfile::TempDir;

const METADATA: &str = r#"{"packages": [
    {"name": "core", "publish": null, "targets": [{"kind": ["lib"]}]},
    {"name": "fresh", "publish": null, "targets": [{"kind": ["lib"]}]},
    {"name": "app", "publish": null, "targets": [{"kind": ["lib"]}, {"kind": ["bin"]}]},
    {"name": "internal", "publish": [], "targets": [{"kind": ["lib"]}]}
]}"#;

/// A stub `cargo` that answers `metadata`, reports `fresh` missing from
/// crates.io, fails lookups named in `LOOKUP_FAILS`, and records each call.
struct Fixture {
    tools: TempDir,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let tools = tempfile::tempdir()?;
        fs::write(tools.path().join("metadata.json"), METADATA)?;
        let executable = tools.path().join("cargo");
        fs::write(
            &executable,
            r#"#!/bin/sh
echo "$*" >> "$CARGO_CALLS"
case "$1" in
metadata)
  [ -n "$METADATA_FAILS" ] && { echo "metadata broke" >&2; exit 101; }
  cat "$CARGO_METADATA" ;;
info)
  [ "$4" = "fresh" ] && { echo "error: could not find \`fresh\` in registry \`crates-io\`" >&2; exit 101; }
  [ "$4" = "$LOOKUP_FAILS" ] && { echo "error: index timed out" >&2; exit 101; }
  exit 0 ;;
semver-checks)
  exit "${SEMVER_EXIT:-0}" ;;
esac
"#,
        )?;
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755))?;

        Ok(Self { tools })
    }

    fn run(&self, arguments: &[&str], environment: &[(&str, &str)]) -> std::io::Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .arg("semver-checks")
            .args(arguments)
            .env("CARGO_CALLS", self.tools.path().join("calls"))
            .env("CARGO_METADATA", self.tools.path().join("metadata.json"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.tools.path().display()),
            )
            .envs(environment.iter().copied())
            .output()
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.tools.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

#[test]
fn checks_published_libraries_and_skips_new_crates() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let output = fixture.run(&["--exclude", "app"], &[])?;
    // Assert
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Skipping fresh: new crate"));
    assert_eq!(
        fixture.calls(),
        [
            "metadata --no-deps --format-version 1",
            "info --registry crates-io core",
            "info --registry crates-io fresh",
            "semver-checks --package core",
        ]
    );

    Ok(())
}

#[test]
fn skips_semver_checks_when_no_crate_has_a_baseline() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let output = fixture.run(&["--exclude", "app", "--exclude", "core"], &[])?;
    // Assert
    assert!(output.status.success(), "{output:?}");
    assert!(
        fixture
            .calls()
            .iter()
            .all(|call| !call.starts_with("semver-checks"))
    );

    Ok(())
}

#[test]
fn reports_semver_failures_and_lookup_errors() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let breaking = fixture.run(&[], &[("SEMVER_EXIT", "1")])?;
    let unreachable = fixture.run(&[], &[("LOOKUP_FAILS", "core")])?;
    let unreadable = fixture.run(&[], &[("METADATA_FAILS", "1")])?;
    let unknown = fixture.run(&["--exclude", "missing"], &[])?;
    let without_cargo = fixture.run(&[], &[("PATH", "/nonexistent")])?;
    // Assert
    assert!(!breaking.status.success());
    assert!(String::from_utf8_lossy(&breaking.stdout).contains("reported failures"));
    assert!(!unreachable.status.success());
    assert!(String::from_utf8_lossy(&unreachable.stdout).contains("index timed out"));
    assert!(!unreadable.status.success());
    assert!(String::from_utf8_lossy(&unreadable.stdout).contains("metadata broke"));
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stdout).contains("not a workspace package"));
    assert!(!without_cargo.status.success());
    assert!(String::from_utf8_lossy(&without_cargo.stdout).contains("Could not execute cargo"));

    Ok(())
}
