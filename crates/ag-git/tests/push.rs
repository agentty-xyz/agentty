//! Public push results distinguish remote updates from successful no-ops.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

use ag_git::{GitClient, RealGitClient};
use tempfile::tempdir;

#[tokio::test]
async fn explicit_push_reports_updates_and_already_current_remote() -> Result<(), Box<dyn Error>> {
    // Arrange
    let local = tempdir()?;
    let remote = tempdir()?;
    run_git(local.path(), &["init", "-b", "main"])?;
    run_git(local.path(), &["config", "user.email", "test@example.com"])?;
    run_git(local.path(), &["config", "user.name", "Test"])?;
    fs::write(local.path().join("file.txt"), "initial\n")?;
    run_git(local.path(), &["add", "."])?;
    run_git(local.path(), &["commit", "-m", "Initial"])?;
    run_git(remote.path(), &["init", "--bare"])?;
    run_git(
        local.path(),
        &["remote", "add", "review", &remote.path().to_string_lossy()],
    )?;
    run_git(local.path(), &["config", "branch.main.remote", "review"])?;
    let client = RealGitClient;

    for (content, expected_update) in [
        (None, true),
        (None, false),
        (Some("changed\n"), true),
        (None, false),
    ] {
        if let Some(content) = content {
            fs::write(local.path().join("file.txt"), content)?;
            run_git(local.path(), &["commit", "-am", "Update"])?;
        }

        // Act
        let outcome = client
            .push_current_branch_to_remote_branch_with_outcome(
                local.path().to_path_buf(),
                "review/topic".to_string(),
            )
            .await?;

        // Assert
        assert_eq!(outcome.updated, expected_update);
        assert_eq!(outcome.upstream_reference, "review/review/topic");
        assert_eq!(
            run_git(local.path(), &["rev-parse", "HEAD"])?,
            run_git(remote.path(), &["rev-parse", "refs/heads/review/topic"])?,
        );
    }

    Ok(())
}

fn run_git(repository: &Path, arguments: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(arguments)
        .current_dir(repository)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
