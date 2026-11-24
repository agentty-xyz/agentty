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

#[cfg(unix)]
#[tokio::test]
async fn push_runs_no_pre_push_hook_wherever_it_is_installed() -> Result<(), Box<dyn Error>> {
    // Arrange
    let root = tempdir()?;
    let local = root.path().join("local");
    let remote = root.path().join("remote");
    let marker = root.path().join("pre-push-ran");
    fs::create_dir(&local)?;
    run_git(&local, &["init", "-b", "main"])?;
    run_git(&local, &["config", "user.email", "test@example.com"])?;
    run_git(&local, &["config", "user.name", "Test"])?;
    fs::write(local.join("file.txt"), "initial\n")?;
    run_git(&local, &["add", "."])?;
    run_git(&local, &["commit", "-m", "Initial"])?;
    run_git(root.path(), &["init", "--bare", "remote"])?;
    run_git(
        &local,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    )?;
    let hook = format!("#!/bin/sh\necho \"$0\" >> '{}'\n", marker.display());
    let worktree_hooks = local.join(".githooks");
    let trusted_hooks = root.path().join("trusted-hooks");
    for directory in [&worktree_hooks, &trusted_hooks] {
        fs::create_dir(directory)?;
        let path = directory.join("pre-push");
        fs::write(&path, &hook)?;
        let mut permissions = fs::metadata(&path)?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        fs::set_permissions(&path, permissions)?;
    }
    let client = RealGitClient;

    // Act
    run_git(&local, &["config", "core.hooksPath", ".githooks"])?;
    client
        .push_current_branch_to_remote_branch(local.clone(), "worktree-hooks".to_string())
        .await?;
    let after_worktree_hooks = fs::read_to_string(&marker).unwrap_or_default();
    run_git(
        &local,
        &["config", "core.hooksPath", &trusted_hooks.to_string_lossy()],
    )?;
    client
        .push_current_branch_to_remote_branch(local.clone(), "trusted-hooks".to_string())
        .await?;
    let after_trusted_hooks = fs::read_to_string(&marker).unwrap_or_default();

    // Assert
    assert_eq!(after_worktree_hooks, "");
    assert_eq!(after_trusted_hooks, "");
    // Positive control: plain Git runs the configured hook.
    let plain = Command::new("git")
        .args(["push", "-q", "origin", "HEAD:plain"])
        .current_dir(&local)
        .status()?;
    assert!(plain.success());
    assert!(fs::read_to_string(&marker)?.contains("trusted-hooks/pre-push"));

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
