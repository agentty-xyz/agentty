//! Public affected-package Clippy CLI contracts with an injected Cargo.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    /// Creates a workspace where `agentty` has a development dependency on
    /// `core`, while `other` stands alone. A `cargo` stub records Clippy
    /// arguments.
    fn new(metadata: Option<&str>) -> Result<Self, Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let root = root.display();
        let metadata = metadata.map_or_else(
            || {
                format!(
                    r#"{{"target_directory":"target","workspace_members":["core","agentty","other"],"packages":[{{"id":"core","name":"core","manifest_path":"{root}/crates/core/Cargo.toml","dependencies":[]}},{{"id":"agentty","name":"agentty","manifest_path":"{root}/crates/agentty/Cargo.toml","dependencies":[{{"name":"core","path":"{root}/crates/core","kind":"dev"}}]}},{{"id":"other","name":"other","manifest_path":"{root}/crates/other/Cargo.toml","dependencies":[]}}]}}"#
                )
            },
            str::to_string,
        );
        fs::write(directory.path().join("metadata.json"), metadata)?;
        let cargo = directory.path().join("cargo");
        fs::write(
            &cargo,
            "#!/bin/sh\nif [ \"$1\" = metadata ]; then cat metadata.json; else echo \"$*\" >> \
             calls; case \" $* \" in *\" --test \"*) exit \
             \"${PTY_CLIPPY_EXIT:-${CLIPPY_EXIT:-0}}\";; *) exit \"${CLIPPY_EXIT:-0}\";; esac; \
             fi\n",
        )?;
        fs::set_permissions(cargo, fs::Permissions::from_mode(0o755))?;

        Ok(Self { directory })
    }

    /// Replaces the stub with one that answers `cargo metadata` and then
    /// deletes itself, so launching Clippy afterwards fails.
    fn remove_cargo_after_metadata(&self) -> Result<(), Box<dyn Error>> {
        let cargo = self.directory.path().join("cargo");
        fs::write(
            &cargo,
            "#!/bin/sh\n/bin/cat metadata.json\n/bin/rm \"$0\"\n",
        )?;
        fs::set_permissions(cargo, fs::Permissions::from_mode(0o755))?;

        Ok(())
    }

    fn run(&self, files: &[&str], exit_code: i32) -> std::io::Result<Output> {
        self.command(files, exit_code)?.output()
    }

    fn command(&self, files: &[&str], exit_code: i32) -> std::io::Result<Command> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ag-xtask"));
        command
            .arg("clippy")
            .args(files)
            .current_dir(self.directory.path().canonicalize()?)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.directory.path().display()),
            )
            .env("CLIPPY_EXIT", exit_code.to_string());
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }

        Ok(command)
    }

    fn calls(&self) -> Result<Vec<String>, Box<dyn Error>> {
        Ok(fs::read_to_string(self.directory.path().join("calls"))?
            .lines()
            .map(str::to_string)
            .collect())
    }
}

#[test]
fn package_changes_lint_owners_and_dependents_and_root_files_lint_the_workspace()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new(None)?;
    // Act
    let package_change = fixture.run(&["crates/core/src/lib.rs"], 0)?;
    let standalone_change = fixture.run(&["crates/other/src/lib.rs"], 0)?;
    let lockfile_change = fixture.run(&["crates/other/src/lib.rs", "Cargo.lock"], 0)?;
    // Assert
    assert!(package_change.status.success());
    assert!(standalone_change.status.success());
    assert!(lockfile_change.status.success());
    let flags = "-q --locked --all-targets --all-features";
    assert_eq!(
        fixture.calls()?,
        [
            format!("clippy {flags} -p agentty -p core -- -D warnings"),
            "clippy -q --locked -p agentty --test e2e --test protocol_compliance_e2e --test \
             showcase -- -D warnings"
                .to_string(),
            format!("clippy {flags} -p other -- -D warnings"),
            format!("clippy {flags} --workspace -- -D warnings"),
            "clippy -q --locked -p agentty --test e2e --test protocol_compliance_e2e --test \
             showcase -- -D warnings"
                .to_string(),
        ]
    );

    Ok(())
}

#[test]
fn coordinated_packages_override_catalog_filenames_and_reject_invalid_selections()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new(None)?;
    let files = ["Cargo.toml", "crates/other/src/lib.rs"];

    // Act
    let selected = fixture
        .command(&files, 0)?
        .env("AGENTTY_CLIPPY_PACKAGES", " core\t\n")
        .output()?;
    let empty = fixture
        .command(&files, 0)?
        .env("AGENTTY_CLIPPY_PACKAGES", " \t")
        .output()?;
    let unknown = fixture
        .command(&files, 0)?
        .env("AGENTTY_CLIPPY_PACKAGES", "missing")
        .output()?;

    // Assert
    assert!(selected.status.success());
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stdout).contains("must contain package names"));
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stdout).contains("Unknown workspace package"));
    assert_eq!(
        fixture.calls()?,
        [
            "clippy -q --locked --all-targets --all-features -p agentty -p core -- -D warnings",
            "clippy -q --locked -p agentty --test e2e --test protocol_compliance_e2e --test \
             showcase -- -D warnings"
        ]
    );

    Ok(())
}

#[test]
fn clippy_failures_and_unreadable_metadata_fail_the_command() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new(None)?;
    let broken = Fixture::new(Some("not metadata"))?;
    // Act
    let lint_failure = fixture.run(&["crates/core/src/lib.rs"], 1)?;
    let metadata_failure = broken.run(&["crates/core/src/lib.rs"], 0)?;
    // Assert
    assert!(!lint_failure.status.success());
    assert!(!metadata_failure.status.success());
    assert!(
        broken.calls().is_err(),
        "Clippy must not run without metadata"
    );

    Ok(())
}

#[test]
fn cargo_parent_context_does_not_leak_into_clippy() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new(None)?;
    fs::write(
        fixture.directory.path().join("cargo"),
        r#"#!/bin/sh
if [ "$1" = metadata ]; then
    cat metadata.json
else
    printf '%s\n' "${CARGO_MANIFEST_DIR-unset}" "${CARGO_MANIFEST_PATH-unset}" \
        "${CARGO_MANIFEST_LINKS-unset}" "${CARGO_PKG_NAME-unset}" \
        "${CARGO_PKG_VERSION_MAJOR-unset}" "$AGENTTY_KEEP_ENV" "$CARGO_HOME" >> environment
    echo "$*" >> calls
fi
"#,
    )?;

    // Act
    let output = fixture
        .command(&["crates/core/src/lib.rs"], 0)?
        .env("CARGO_MANIFEST_DIR", "/cargo-parent/ag-xtask")
        .env("CARGO_MANIFEST_PATH", "/cargo-parent/ag-xtask/Cargo.toml")
        .env("CARGO_MANIFEST_LINKS", "parent-links")
        .env("CARGO_PKG_NAME", "ag-xtask")
        .env("CARGO_PKG_VERSION_MAJOR", "0")
        .env("AGENTTY_KEEP_ENV", "preserved")
        .env("CARGO_HOME", "unchanged-cache")
        .output()?;

    // Assert
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(fixture.directory.path().join("environment"))?,
        "unset\nunset\nunset\nunset\nunset\npreserved\nunchanged-cache\n".repeat(2)
    );
    assert_eq!(
        fixture.calls()?,
        [
            "clippy -q --locked --all-targets --all-features -p agentty -p core -- -D warnings",
            "clippy -q --locked -p agentty --test e2e --test protocol_compliance_e2e --test \
             showcase -- -D warnings"
        ]
    );

    Ok(())
}

#[test]
fn cargo_that_cannot_launch_clippy_fails_the_command() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new(None)?;
    fixture.remove_cargo_after_metadata()?;
    // Act
    let output = fixture
        .command(&["crates/core/src/lib.rs"], 0)?
        .env("PATH", fixture.directory.path())
        .output()?;
    // Assert
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Could not execute cargo"));

    Ok(())
}

#[test]
fn pty_lint_includes_development_dependencies_and_shared_files() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new(None)?;

    // Act
    for files in [
        vec!["--agentty-e2e", "crates/agentty/src/lib.rs"],
        vec!["--agentty-e2e", "crates/core/src/lib.rs"],
        vec!["--agentty-e2e", "Cargo.lock"],
        vec!["--agentty-e2e"],
    ] {
        let result = fixture.run(&files, 0)?;

        // Assert
        assert!(result.status.success(), "{result:?}");
    }
    assert_eq!(
        fixture.calls()?,
        vec![
            "clippy -q --locked -p agentty --test e2e --test protocol_compliance_e2e --test \
             showcase -- -D warnings";
            4
        ]
    );

    Ok(())
}

#[test]
fn pty_lint_skips_unrelated_packages_and_respects_coordinator_scope() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let fixture = Fixture::new(None)?;

    // Act
    let unrelated = fixture.run(&["--agentty-e2e", "crates/other/src/lib.rs"], 1)?;
    let scoped = fixture
        .command(&["--agentty-e2e", "Cargo.toml"], 1)?
        .env("AGENTTY_CLIPPY_PACKAGES", "other")
        .output()?;

    // Assert
    assert!(unrelated.status.success());
    assert!(scoped.status.success());
    assert!(fixture.calls().is_err(), "unaffected targets must not run");

    // Act
    let affected = fixture
        .command(&["--agentty-e2e", "crates/other/src/lib.rs"], 1)?
        .env("AGENTTY_CLIPPY_PACKAGES", "core")
        .output()?;

    // Assert
    assert!(!affected.status.success());
    assert_eq!(
        fixture.calls()?.len(),
        1,
        "affected lint failures must propagate"
    );

    Ok(())
}

#[test]
fn combined_lint_runs_both_commands_and_preserves_either_failure() -> Result<(), Box<dyn Error>> {
    // Arrange
    for (regular_exit, pty_exit) in [(1, 0), (0, 1)] {
        let fixture = Fixture::new(None)?;

        // Act
        let output = fixture
            .command(&["crates/core/src/lib.rs"], regular_exit)?
            .env("PTY_CLIPPY_EXIT", pty_exit.to_string())
            .output()?;

        // Assert
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(
            fixture.calls()?.len(),
            2,
            "collect failures from both lint selections"
        );
    }

    Ok(())
}
