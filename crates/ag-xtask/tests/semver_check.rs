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
    {"name": "store", "publish": null, "targets": [{"kind": ["lib"]}]},
    {"name": "app", "publish": null, "targets": [{"kind": ["lib"]}, {"kind": ["bin"]}]},
    {"name": "internal", "publish": [], "targets": [{"kind": ["lib"]}]}
]}"#;

/// A stub `cargo` that answers `metadata`, reports `fresh` missing from
/// crates.io, fails lookups named in `LOOKUP_FAILS`, and records each call.
/// Its `semver-checks` reports the crate named in `BROKEN_BASELINE`,
/// `BROKEN_CURRENT`, or `BREAKING` the way `cargo semver-checks` does.
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
  for name; do :; done
  echo "    Building $name v1.0.0 (current)" >&2
  if [ "$name" = "$BROKEN_CURRENT" ]; then
    echo "error: failed to build rustdoc for crate $name v1.0.0" >&2; exit 1
  fi
  echo "    Building $name v1.0.0 (baseline)" >&2
  if [ "$name" = "$BROKEN_BASELINE" ]; then
    echo "error: failed to build rustdoc for crate $name v1.0.0" >&2; exit 1
  fi
  if [ "$name" = "$BREAKING" ]; then
    echo "     Summary semver requires new major version" >&2; exit 1
  fi
  echo "     Summary no semver update required" >&2 ;;
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
fn checks_published_libraries_one_at_a_time_and_skips_new_crates() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let output = fixture.run(&["--exclude", "app"], &[])?;
    // Assert
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Skipping fresh: new crate"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Building store v1.0.0 (baseline)"));
    assert_eq!(
        fixture.calls(),
        [
            "metadata --no-deps --format-version 1",
            "info --registry crates-io core",
            "semver-checks --color never --package core",
            "info --registry crates-io fresh",
            "info --registry crates-io store",
            "semver-checks --color never --package store",
        ]
    );

    Ok(())
}

#[test]
fn passes_an_explicit_release_type_to_semver_checks() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let output = fixture.run(
        &[
            "--exclude",
            "app",
            "--exclude",
            "store",
            "--release-type",
            "major",
        ],
        &[],
    )?;
    // Assert
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fixture
            .calls()
            .iter()
            .filter(|call| call.starts_with("semver-checks"))
            .collect::<Vec<_>>(),
        ["semver-checks --color never --release-type major --package core"]
    );

    Ok(())
}

#[test]
fn skips_a_baseline_that_does_not_build_with_a_warning() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let output = fixture.run(&["--exclude", "app"], &[("BROKEN_BASELINE", "core")])?;
    // Assert
    assert!(output.status.success(), "{output:?}");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(log.contains("Skipping core: its newest crates.io release does not build"));
    assert!(
        fixture
            .calls()
            .iter()
            .any(|call| call == "semver-checks --color never --package store")
    );

    Ok(())
}

#[test]
fn reports_each_failing_crate_by_cause() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let output = fixture.run(
        &["--exclude", "app"],
        &[("BROKEN_CURRENT", "core"), ("BREAKING", "store")],
    )?;
    // Assert
    assert!(!output.status.success());
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(log.contains("core: the workspace crate's rustdoc does not build"));
    assert!(log.contains("store: public API changes need a larger version bump"));

    Ok(())
}

#[test]
fn skips_semver_checks_when_no_crate_has_a_baseline() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let output = fixture.run(
        &[
            "--exclude",
            "app",
            "--exclude",
            "core",
            "--exclude",
            "store",
        ],
        &[],
    )?;
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
fn reports_lookup_metadata_and_execution_errors() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let unreachable = fixture.run(&[], &[("LOOKUP_FAILS", "core")])?;
    let unreadable = fixture.run(&[], &[("METADATA_FAILS", "1")])?;
    let unknown = fixture.run(&["--exclude", "missing"], &[])?;
    let without_cargo = fixture.run(&[], &[("PATH", "/nonexistent")])?;
    // Assert
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
