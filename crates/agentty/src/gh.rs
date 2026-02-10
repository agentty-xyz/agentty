use std::path::Path;
use std::process::Command;

use serde::Deserialize;

/// Creates a draft pull request for `source_branch` against `target_branch`.
///
/// # Errors
/// Returns an error if `gh pr create` fails and no existing PR URL can be
/// retrieved.
pub(crate) fn create_pull_request(
    repo_path: &Path,
    source_branch: &str,
    target_branch: &str,
    title: &str,
) -> Result<String, String> {
    let output = Command::new("gh")
        .args([
            "pr",
            "create",
            "--draft",
            "--base",
            target_branch,
            "--head",
            source_branch,
            "--title",
            title,
            "--body",
            "Created by Agentty",
        ])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute gh pr create: {error}"))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("already exists")
        && let Ok(url) = pull_request_url(repo_path, source_branch)
    {
        return Ok(format!("Updated existing PR: {url}"));
    }

    Err(format!("GitHub CLI failed: {}", stderr.trim()))
}

/// Returns whether the pull request for `source_branch` has been merged.
///
/// # Errors
/// Returns an error if `gh pr view` fails or returns invalid JSON.
pub(crate) fn is_pull_request_merged(
    repo_path: &Path,
    source_branch: &str,
) -> Result<bool, String> {
    let state = pull_request_state(repo_path, source_branch)?;

    Ok(state == PullRequestState::Merged)
}

/// Returns whether the pull request for `source_branch` was closed without
/// merge.
///
/// # Errors
/// Returns an error if `gh pr view` fails or returns invalid JSON.
pub(crate) fn is_pull_request_closed(
    repo_path: &Path,
    source_branch: &str,
) -> Result<bool, String> {
    let state = pull_request_state(repo_path, source_branch)?;

    Ok(state == PullRequestState::Closed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum PullRequestState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Deserialize)]
struct PullRequestView {
    state: Option<PullRequestState>,
    url: Option<String>,
}

fn pull_request_url(repo_path: &Path, source_branch: &str) -> Result<String, String> {
    let view = pull_request_view(repo_path, source_branch, "url")?;

    view.url
        .ok_or_else(|| "Missing `url` in gh pr view response".to_string())
}

fn pull_request_state(repo_path: &Path, source_branch: &str) -> Result<PullRequestState, String> {
    let view = pull_request_view(repo_path, source_branch, "state")?;

    view.state
        .ok_or_else(|| "Missing `state` in gh pr view response".to_string())
}

fn pull_request_view(
    repo_path: &Path,
    source_branch: &str,
    fields: &str,
) -> Result<PullRequestView, String> {
    let output = Command::new("gh")
        .args(["pr", "view", source_branch, "--json", fields])
        .current_dir(repo_path)
        .output()
        .map_err(|error| format!("Failed to execute gh pr view: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);

        return Err(format!("GitHub CLI failed: {}", stderr.trim()));
    }

    parse_pull_request_view(&output.stdout)
}

fn parse_pull_request_view(stdout: &[u8]) -> Result<PullRequestView, String> {
    serde_json::from_slice::<PullRequestView>(stdout)
        .map_err(|error| format!("Failed to parse gh pr view JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pull_request_view_state() {
        // Arrange
        let payload = br#"{"state":"MERGED"}"#;

        // Act
        let view = parse_pull_request_view(payload).expect("failed to parse payload");

        // Assert
        assert_eq!(view.state, Some(PullRequestState::Merged));
        assert_eq!(view.url, None);
    }

    #[test]
    fn test_parse_pull_request_view_url() {
        // Arrange
        let payload = br#"{"url":"https://github.com/opencloudtool/agentty/pull/1"}"#;

        // Act
        let view = parse_pull_request_view(payload).expect("failed to parse payload");

        // Assert
        assert_eq!(
            view.url,
            Some("https://github.com/opencloudtool/agentty/pull/1".to_string())
        );
        assert_eq!(view.state, None);
    }

    #[test]
    fn test_parse_pull_request_view_invalid_json() {
        // Arrange
        let payload = br"not-json";

        // Act
        let result = parse_pull_request_view(payload);

        // Assert
        assert!(result.is_err());
    }
}
