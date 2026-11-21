//! Public release-check decision CLI contracts over a fixture repository.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const MANIFEST: &str = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.1.0\"\n";

/// A Git repository whose `main` branch holds the base manifest, checked out
/// on a `change` branch.
struct Fixture {
    output: TempDir,
    repository: TempDir,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let fixture = Self {
            output: tempfile::tempdir()?,
            repository: tempfile::tempdir()?,
        };
        fixture.write("Cargo.toml", MANIFEST)?;
        fixture.write("notes.md", "base")?;
        fixture.git(&["init", "-q", "-b", "main"])?;
        fixture.git(&["add", "."])?;
        fixture.git(&["commit", "-qm", "base"])?;
        fixture.git(&["switch", "-qc", "change"])?;

        Ok(fixture)
    }

    fn write(&self, path: &str, content: &str) -> Result<(), Box<dyn Error>> {
        let path = self.repository.path().join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;

        Ok(())
    }

    fn git(&self, arguments: &[&str]) -> Result<(), Box<dyn Error>> {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(arguments)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(self.repository.path())
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        Ok(())
    }

    fn output_path(&self) -> PathBuf {
        self.output.path().join("github-output")
    }

    fn run(&self, base: &str, output: &Path, path: &str) -> std::io::Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .args(["release-gate", "--base", base, "--output"])
            .arg(output)
            .current_dir(self.repository.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("PATH", path)
            .output()
    }

    fn decide(&self) -> Result<String, Box<dyn Error>> {
        let output = self.run("main", &self.output_path(), "/usr/bin:/bin")?;
        assert!(output.status.success(), "{output:?}");

        Ok(fs::read_to_string(self.output_path())?)
    }
}

#[test]
fn a_version_bump_runs_the_release_checks() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.write("Cargo.toml", &MANIFEST.replace("0.1.0", "0.1.1"))?;
    fixture.git(&["commit", "-qam", "bump"])?;
    // Act
    let decision = fixture.decide()?;
    // Assert
    assert_eq!(decision, "run=true\nrelease-type=\n");

    Ok(())
}

#[test]
fn a_changed_check_input_runs_the_release_checks() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.write(
        "rust-toolchain.toml",
        "[toolchain]\nchannel = \"nightly\"\n",
    )?;
    // Act
    let decision = fixture.decide()?;
    // Assert
    assert_eq!(decision, "run=true\nrelease-type=major\n");

    Ok(())
}

#[test]
fn other_changes_skip_the_release_checks() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.write("notes.md", "changed")?;
    // Act
    let decision = fixture.decide()?;
    // Assert
    assert_eq!(decision, "run=false\nrelease-type=\n");

    Ok(())
}

#[test]
fn unreadable_inputs_fail_instead_of_skipping() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    let output = fixture.output_path();
    let path = "/usr/bin:/bin";
    // Act
    let unknown_base = fixture.run("missing", &output, path)?;
    let without_git = fixture.run("main", &output, "/nonexistent")?;
    let unwritable = fixture.run("main", &fixture.output.path().join("absent/file"), path)?;
    fixture.write("Cargo.toml", "[workspace]\nmembers = []\n")?;
    let unversioned = fixture.run("main", &output, path)?;
    fs::remove_file(fixture.repository.path().join("Cargo.toml"))?;
    let deleted = fixture.run("main", &output, path)?;
    // Assert
    for (result, message) in [
        (&unknown_base, "git merge-base"),
        (&without_git, "Could not execute git"),
        (&unwritable, "Could not write"),
        (&unversioned, "no [workspace.package].version"),
        (&deleted, "Could not read the workspace Cargo.toml"),
    ] {
        assert!(!result.status.success(), "{result:?}");
        assert!(
            String::from_utf8_lossy(&result.stdout).contains(message),
            "{message}: {result:?}"
        );
    }
    assert!(!output.exists());

    Ok(())
}
