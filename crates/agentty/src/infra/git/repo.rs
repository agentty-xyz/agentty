use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tokio::task::spawn_blocking;

/// Returns the origin repository URL normalized to HTTPS form when possible.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
///
/// # Returns
/// Ok(url) on success, Err(msg) with detailed error message on failure
///
/// # Errors
/// Returns an error if the remote URL cannot be read via `git remote get-url`.
pub async fn repo_url(repo_path: PathBuf) -> Result<String, String> {
    spawn_blocking(move || {
        let output = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(&repo_path)
            .output()
            .map_err(|error| format!("Failed to execute git remote get-url: {error}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(format!("Git remote get-url failed: {}", stderr.trim()));
        }

        let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();

        Ok(normalize_repo_url(&remote))
    })
    .await
    .map_err(|error| format!("Join error: {error}"))?
}

/// Resolves the git directory path for a repository root or worktree root.
pub(super) fn resolve_git_dir(repo_dir: &Path) -> Option<PathBuf> {
    let dot_git = repo_dir.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    if dot_git.is_file() {
        let content = fs::read_to_string(&dot_git).ok()?;
        let git_dir_line = content.lines().find(|line| line.starts_with("gitdir:"))?;
        let git_dir_path = git_dir_line.trim_start_matches("gitdir:").trim();
        let git_dir = PathBuf::from(git_dir_path);

        if git_dir.is_absolute() {
            return Some(git_dir);
        }

        return Some(repo_dir.join(git_dir));
    }

    None
}

/// Extracts the best human-readable error detail from command output.
pub(super) fn command_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr_text.is_empty() {
        return stderr_text;
    }

    let stdout_text = String::from_utf8_lossy(stdout).trim().to_string();
    if !stdout_text.is_empty() {
        return stdout_text;
    }

    "Unknown git error".to_string()
}

/// Converts SSH-style GitHub remotes into HTTPS while preserving other URLs.
fn normalize_repo_url(remote: &str) -> String {
    let trimmed = remote.trim_end_matches(".git");
    if let Some(path) = trimmed.strip_prefix("git@github.com:") {
        return format!("https://github.com/{path}");
    }

    if let Some(path) = trimmed.strip_prefix("ssh://git@github.com/") {
        return format!("https://github.com/{path}");
    }

    trimmed.to_string()
}
