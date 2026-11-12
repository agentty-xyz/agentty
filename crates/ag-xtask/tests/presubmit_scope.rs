//! Process-level contracts for the Presubmit change scope that decides whether
//! a pull request runs the E2E suite.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

use serde_yaml_ng::Value;
use tempfile::TempDir;

/// Records its arguments and prints the saved NUL-separated diff, or fails
/// like `git` does when a revision is missing.
const GIT_STUB: &str = r#"#!/bin/sh
printf '%s\n' "$*" > git-arguments
if [ ! -f diff-output ]; then
  echo "fatal: ambiguous argument 'HEAD^1': unknown revision" >&2
  exit 128
fi
cat diff-output
"#;

/// Changes that cannot affect the suite.
const DOCUMENTATION_PATHS: &[&str] = &[
    "README.md",
    "CHANGELOG.md",
    "AGENTS.md",
    "docs/contributing/validation.md",
    "docs/site/content/docs/usage.md",
    "skills/bump-version/SKILL.md",
    ".github/pull_request_template.md",
    ".github/ISSUE_TEMPLATE/bug.yml",
    ".github/CODEOWNERS",
    "LICENSE",
];

struct Fixture {
    directory: TempDir,
    script: String,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let workflow: Value =
            serde_yaml_ng::from_str(include_str!("../../../.github/workflows/presubmit.yml"))?;
        let script = workflow["jobs"]["scope"]["steps"]
            .as_sequence()
            .ok_or("steps")?
            .iter()
            .find(|step| step["id"].as_str() == Some("scope"))
            .ok_or("scope step")?["run"]
            .as_str()
            .ok_or("run command")?
            .to_string();
        let directory = tempfile::tempdir()?;
        fs::create_dir(directory.path().join("bin"))?;
        fs::create_dir(directory.path().join("runner-temp"))?;
        let git = directory.path().join("bin/git");
        fs::write(&git, GIT_STUB)?;
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755))?;

        Ok(Self { directory, script })
    }

    /// Saves the paths the stubbed diff reports, NUL-terminated as `-z` does.
    fn save_changed_paths(&self, paths: &[&str]) -> std::io::Result<()> {
        let output: String = paths.iter().flat_map(|path| [*path, "\0"]).collect();

        fs::write(self.directory.path().join("diff-output"), output)
    }

    /// Runs the step with GitHub's default `bash` options.
    fn run(&self, event_name: &str) -> std::io::Result<Output> {
        Command::new("bash")
            .args([
                "--noprofile",
                "--norc",
                "-eo",
                "pipefail",
                "-c",
                &self.script,
            ])
            .current_dir(self.directory.path())
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin",
                    self.directory.path().join("bin").display()
                ),
            )
            .env("EVENT_NAME", event_name)
            .env("GITHUB_OUTPUT", self.directory.path().join("github-output"))
            .env("RUNNER_TEMP", self.directory.path().join("runner-temp"))
            .output()
    }

    fn output(&self) -> std::io::Result<String> {
        fs::read_to_string(self.directory.path().join("github-output"))
    }

    fn git_arguments(&self) -> std::io::Result<String> {
        fs::read_to_string(self.directory.path().join("git-arguments"))
    }
}

#[test]
fn documentation_only_pull_request_skips_the_suite() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.save_changed_paths(DOCUMENTATION_PATHS)?;

    // Act
    let result = fixture.run("pull_request")?;

    // Assert
    assert!(result.status.success(), "{result:?}");
    assert_eq!(fixture.output()?, "e2e=false\n");
    assert_eq!(
        fixture.git_arguments()?,
        "diff --name-only --no-renames -z HEAD^1 HEAD\n"
    );
    assert!(String::from_utf8(result.stdout)?.contains("E2E skipped"));

    Ok(())
}

#[test]
fn any_path_the_suite_can_read_runs_it() -> Result<(), Box<dyn Error>> {
    for path in [
        "docs/site/static/features/session_open.gif",
        "docs/site/static/features/session_open.gif.hash",
        "docs/site/static/demo/demo.gif",
        "docs/site/content/features/sessions.md",
        "crates/agentty/src/template/read_only_chat_prompt.md",
        "crates/agentty/AGENTS.md",
        "Cargo.lock",
        ".config/nextest.toml",
        ".github/workflows/e2e.yml",
        "container/e2e.Containerfile",
        "sonar-project.properties",
    ] {
        // Arrange
        let fixture = Fixture::new()?;
        let mut paths = DOCUMENTATION_PATHS.to_vec();
        paths.push(path);
        fixture.save_changed_paths(&paths)?;

        // Act
        let result = fixture.run("pull_request")?;

        // Assert
        assert!(result.status.success(), "{path}: {result:?}");
        assert_eq!(fixture.output()?, "e2e=true\n", "{path}");
        assert!(
            String::from_utf8(result.stdout)?.contains(&format!("E2E runs: {path}")),
            "{path}"
        );
    }

    Ok(())
}

#[test]
fn merge_queue_candidate_runs_the_suite_without_a_diff() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.save_changed_paths(DOCUMENTATION_PATHS)?;

    // Act
    let result = fixture.run("merge_group")?;

    // Assert
    assert!(result.status.success(), "{result:?}");
    assert_eq!(fixture.output()?, "e2e=true\n");
    assert!(!fixture.directory.path().join("git-arguments").exists());

    Ok(())
}

#[test]
fn failed_diff_fails_without_a_decision() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;

    // Act
    let result = fixture.run("pull_request")?;

    // Assert
    assert!(!result.status.success());
    assert!(!fixture.directory.path().join("github-output").exists());
    assert!(String::from_utf8(result.stdout)?.contains("::error title=Change scope::"));

    Ok(())
}
