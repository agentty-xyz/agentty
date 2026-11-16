//! Public changed-file formatting CLI contracts with injected formatters.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

#[path = "support/hook_catalog.rs"]
mod hook_catalog;

/// A Git repository whose `origin/main` holds the base commit, plus stub
/// formatters outside the repository that record their arguments.
struct Fixture {
    repository: TempDir,
    tools: TempDir,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let repository = tempfile::tempdir()?;
        let tools = tempfile::tempdir()?;
        let root = repository.path();
        fs::create_dir(root.join("src"))?;
        fs::write(root.join("src/lib.rs"), "base")?;
        fs::write(root.join("src/old.rs"), "base")?;
        fs::write(root.join("notes.md"), "base")?;
        fs::write(root.join(".rustfmt.toml"), "edition = \"2024\"\n")?;
        let fixture = Self { repository, tools };
        fixture.git(&["init", "-q", "-b", "main"])?;
        fixture.git(&["add", "."])?;
        fixture.git(&["commit", "-qm", "base"])?;
        fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
        fixture.git(&["switch", "-qc", "change"])?;
        for name in ["cargo", "rustfmt"] {
            fixture.write_tool(name)?;
        }

        Ok(fixture)
    }

    fn write_tool(&self, name: &str) -> Result<(), Box<dyn Error>> {
        let executable = self.tools.path().join(name);
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> \"$FORMAT_CALLS\"\nexit \
                 \"${{FORMAT_EXIT:-0}}\"\n"
            ),
        )?;
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755))?;

        Ok(())
    }

    fn write(&self, path: &str, content: &str) -> Result<(), Box<dyn Error>> {
        fs::write(self.repository.path().join(path), content)?;

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

    fn run(&self, arguments: &[&str], exit_code: i32) -> std::io::Result<Output> {
        self.run_in("", arguments, exit_code)
    }

    fn run_in(
        &self,
        directory: &str,
        arguments: &[&str],
        exit_code: i32,
    ) -> std::io::Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .arg("fmt")
            .args(arguments)
            .current_dir(self.repository.path().join(directory))
            .env("FORMAT_CALLS", self.calls_path())
            .env("FORMAT_EXIT", exit_code.to_string())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.tools.path().display()),
            )
            .output()
    }

    fn calls_path(&self) -> PathBuf {
        self.tools.path().join("calls")
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.calls_path())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn committed_staged_and_untracked_rust_files_are_formatted_and_deleted_ones_skipped()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.write("src/lib.rs", "committed")?;
    fixture.git(&["commit", "-qam", "change"])?;
    fixture.write("src/staged.rs", "staged")?;
    fixture.git(&["add", "src/staged.rs"])?;
    fixture.write("src/new.rs", "untracked")?;
    fixture.write("notes.md", "unstaged")?;
    fs::remove_file(fixture.repository.path().join("src/old.rs"))?;
    // Act
    let output = fixture.run(&[], 0)?;
    // Assert
    assert!(output.status.success(), "{}", combined_output(&output));
    assert_eq!(
        fixture.calls(),
        ["rustfmt --config skip_children=true src/lib.rs src/new.rs src/staged.rs"]
    );

    Ok(())
}

#[test]
fn subdirectory_runs_select_untracked_files_across_the_repository() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fs::create_dir(fixture.repository.path().join("tests"))?;
    fixture.write("src/new.rs", "untracked")?;
    fixture.write("tests/extra.rs", "untracked")?;
    // Act
    let output = fixture.run_in("src", &[], 0)?;
    // Assert
    assert!(output.status.success(), "{}", combined_output(&output));
    assert_eq!(
        fixture.calls(),
        ["rustfmt --config skip_children=true src/new.rs tests/extra.rs"]
    );

    Ok(())
}

#[test]
fn branches_without_rust_changes_run_no_formatter() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.write("notes.md", "unstaged")?;
    // Act
    let output = fixture.run(&[], 0)?;
    // Assert
    assert!(output.status.success(), "{}", combined_output(&output));
    assert!(combined_output(&output).contains("No changed Rust files to format"));
    assert_eq!(fixture.calls(), Vec::<String>::new());

    Ok(())
}

#[test]
fn formatter_configuration_changes_format_the_workspace() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.write("src/lib.rs", "unstaged")?;
    fixture.write(".rustfmt.toml", "edition = \"2024\"\nmax_width = 90\n")?;
    // Act
    let output = fixture.run(&[], 0)?;
    // Assert
    assert!(output.status.success(), "{}", combined_output(&output));
    assert_eq!(fixture.calls(), ["cargo fmt --all"]);

    Ok(())
}

#[test]
fn formatter_failures_missing_formatters_and_unknown_bases_fail() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.write("src/lib.rs", "unstaged")?;
    // Act
    let formatter_failure = fixture.run(&[], 1)?;
    let unknown_base = fixture.run(&["--base", "missing"], 0)?;
    fs::remove_file(fixture.tools.path().join("rustfmt"))?;
    let missing_formatter = fixture.run(&[], 0)?;
    // Assert
    assert!(!formatter_failure.status.success());
    assert!(combined_output(&formatter_failure).contains("rustfmt reported failures"));
    assert!(!unknown_base.status.success());
    assert!(!missing_formatter.status.success());
    assert!(combined_output(&missing_formatter).contains("Could not execute rustfmt"));
    assert_eq!(
        fixture.calls(),
        ["rustfmt --config skip_children=true src/lib.rs"]
    );

    Ok(())
}

#[test]
fn cataloged_fixer_delegates_to_changed_file_formatting() -> Result<(), Box<dyn Error>> {
    // Arrange
    let expected = "cargo run -q --locked -p ag-xtask -- fmt";
    // Act
    let entry = hook_catalog::entry("rustfmt-fix")?;
    // Assert
    assert_eq!(entry, expected);

    Ok(())
}
