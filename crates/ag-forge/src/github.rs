//! GitHub review-request adapter routed through the `gh` CLI.

use std::sync::Arc;

use serde::Deserialize;

use super::{
    CreateReviewRequestInput, ForgeCommand, ForgeCommandOutput, ForgeCommandRunner, ForgeKind,
    ForgeRemote, ReviewComment, ReviewCommentSnapshot, ReviewCommentThread, ReviewRequestError,
    ReviewRequestState, ReviewRequestSummary, command_output_detail,
    looks_like_authentication_failure, looks_like_host_resolution_failure, map_spawn_error,
    normalize_provider_label, parse_remote_url, status_summary_parts, strip_port,
};

/// GitHub pull-request adapter that normalizes `gh` command output.
pub(crate) struct GitHubReviewRequestAdapter {
    command_runner: Arc<dyn ForgeCommandRunner>,
}

impl GitHubReviewRequestAdapter {
    /// Builds one GitHub adapter from a forge command runner.
    pub(crate) fn new(command_runner: Arc<dyn ForgeCommandRunner>) -> Self {
        Self { command_runner }
    }

    /// Returns normalized GitHub remote metadata when `repo_url` is supported.
    pub(crate) fn detect_remote(repo_url: &str) -> Option<ForgeRemote> {
        let parsed_remote = parse_remote_url(repo_url)?;
        if strip_port(&parsed_remote.host) != "github.com" {
            return None;
        }

        Some(parsed_remote.into_forge_remote(ForgeKind::GitHub))
    }

    /// Finds one existing pull request for `source_branch`.
    pub(crate) async fn find_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> Result<Option<ReviewRequestSummary>, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        self.find_by_source_branch_after_auth(remote, source_branch)
            .await
    }

    /// Creates one new draft pull request from `input`.
    pub(crate) async fn create_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        self.create_review_request_after_auth(remote, input).await
    }

    /// Refreshes one existing pull request by display id.
    pub(crate) async fn refresh_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        self.refresh_review_request_after_auth(remote, display_id)
            .await
    }

    /// Fetches the review-comment snapshot for one existing pull request by
    /// display id through GitHub's GraphQL API.
    ///
    /// Returns both inline review threads anchored to diff lines and the
    /// review-request-wide "conversation" comments that are not anchored to a
    /// file or line.
    pub(crate) async fn fetch_review_comment_snapshot(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> Result<ReviewCommentSnapshot, ReviewRequestError> {
        self.ensure_authenticated(&remote).await?;

        let pull_request_number = parse_display_id(&display_id)?;
        let output = self
            .run_review_command(
                &remote,
                review_threads_command(&remote, &pull_request_number),
                "fetch review comments",
            )
            .await?;

        parse_review_comment_snapshot_response(&output.stdout).map_err(|message| {
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message,
            }
        })
    }

    /// Finds one existing pull request after authentication has been verified.
    async fn find_by_source_branch_after_auth(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> Result<Option<ReviewRequestSummary>, ReviewRequestError> {
        let output = self
            .run_review_command(
                &remote,
                lookup_command(&remote, &source_branch),
                "find pull request",
            )
            .await?;
        let display_id = parse_lookup_display_id(&output.stdout).map_err(|message| {
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message,
            }
        })?;

        let Some(display_id) = display_id else {
            return Ok(None);
        };

        self.refresh_review_request_after_auth(remote, display_id)
            .await
            .map(Some)
    }

    /// Creates one new draft pull request after authentication has been
    /// verified.
    async fn create_review_request_after_auth(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        let source_branch = input.source_branch.clone();
        self.run_review_command(
            &remote,
            create_command(&remote, &input),
            "create pull request",
        )
        .await?;

        self.find_by_source_branch_after_auth(remote, source_branch)
            .await?
            .ok_or_else(|| ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message: "GitHub pull request was created but could not be reloaded".to_string(),
            })
    }

    /// Refreshes one pull request after authentication has been verified.
    async fn refresh_review_request_after_auth(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> Result<ReviewRequestSummary, ReviewRequestError> {
        let pull_request_number = parse_display_id(&display_id)?;
        let output = self
            .run_review_command(
                &remote,
                view_command(&remote, &pull_request_number),
                "refresh pull request",
            )
            .await?;

        parse_view_response(&output.stdout).map_err(|message| ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message,
        })
    }

    /// Verifies that `gh` is installed and authenticated for `remote.host`.
    async fn ensure_authenticated(&self, remote: &ForgeRemote) -> Result<(), ReviewRequestError> {
        let output = self
            .command_runner
            .run(auth_status_command(remote))
            .await
            .map_err(|error| map_spawn_error(remote, error))?;
        if output.success() {
            return Ok(());
        }

        if looks_like_host_resolution_failure(&command_output_detail(&output)) {
            return Err(ReviewRequestError::HostResolutionFailed {
                forge_kind: ForgeKind::GitHub,
                host: remote.host.clone(),
            });
        }

        Err(ReviewRequestError::AuthenticationRequired {
            detail: Some(command_output_detail(&output)),
            forge_kind: ForgeKind::GitHub,
            host: remote.host.clone(),
        })
    }

    /// Runs one authenticated `gh` command and normalizes common failures.
    async fn run_review_command(
        &self,
        remote: &ForgeRemote,
        command: ForgeCommand,
        operation: &str,
    ) -> Result<ForgeCommandOutput, ReviewRequestError> {
        let output = self
            .command_runner
            .run(command)
            .await
            .map_err(|error| map_spawn_error(remote, error))?;
        if output.success() {
            return Ok(output);
        }

        let detail = command_output_detail(&output);
        if looks_like_host_resolution_failure(&detail) {
            return Err(ReviewRequestError::HostResolutionFailed {
                forge_kind: ForgeKind::GitHub,
                host: remote.host.clone(),
            });
        }

        if looks_like_authentication_failure(&detail, ForgeKind::GitHub) {
            return Err(ReviewRequestError::AuthenticationRequired {
                detail: Some(detail),
                forge_kind: ForgeKind::GitHub,
                host: remote.host.clone(),
            });
        }

        Err(ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: format!("{operation}: {detail}"),
        })
    }
}

/// Builds the `gh auth status` command for one GitHub host.
fn auth_status_command(remote: &ForgeRemote) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "auth".to_string(),
            "status".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
        ],
    )
}

/// Builds the `gh api` lookup command for `source_branch`.
fn lookup_command(remote: &ForgeRemote, source_branch: &str) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "api".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
            "--method".to_string(),
            "GET".to_string(),
            format!("repos/{}/{}/pulls", remote.namespace, remote.project),
            "-f".to_string(),
            format!("head={}:{}", remote.namespace, source_branch),
            "-f".to_string(),
            "state=all".to_string(),
            "-f".to_string(),
            "sort=created".to_string(),
            "-f".to_string(),
            "direction=desc".to_string(),
            "-f".to_string(),
            "per_page=1".to_string(),
        ],
    )
}

/// Builds the `gh pr create` command for `input`.
///
/// GitHub pull requests default to draft so session-published review requests
/// do not appear ready for merge before the user chooses to mark them ready.
/// When a session worktree is available, the command runs there so `gh` does
/// not inherit a stale process cwd and fail when it shells out to `git`.
fn create_command(remote: &ForgeRemote, input: &CreateReviewRequestInput) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "pr".to_string(),
            "create".to_string(),
            "--draft".to_string(),
            "--repo".to_string(),
            remote.project_path(),
            "--head".to_string(),
            input.source_branch.clone(),
            "--base".to_string(),
            input.target_branch.clone(),
            "--title".to_string(),
            input.title.clone(),
            "--body".to_string(),
            input.body.clone().unwrap_or_default(),
        ],
    )
}

/// Builds one `gh api graphql` command that fetches review threads and
/// comments for `pull_request_number`.
fn review_threads_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "api".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
            "graphql".to_string(),
            "-f".to_string(),
            format!("query={REVIEW_THREADS_QUERY}"),
            "-F".to_string(),
            format!("owner={}", remote.namespace),
            "-F".to_string(),
            format!("repo={}", remote.project),
            "-F".to_string(),
            format!("number={pull_request_number}"),
        ],
    )
}

/// Builds the `gh pr view` command for one pull-request number.
fn view_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "pr".to_string(),
            "view".to_string(),
            pull_request_number.to_string(),
            "--repo".to_string(),
            remote.project_path(),
            "--json".to_string(),
            "number,title,state,url,baseRefName,headRefName,isDraft,mergeStateStatus,\
             reviewDecision,mergedAt"
                .to_string(),
        ],
    )
}

/// Builds one base `gh` command with deterministic color settings and the
/// optional session worktree for repository-aware git fallback commands.
fn github_command(remote: &ForgeRemote, arguments: Vec<String>) -> ForgeCommand {
    ForgeCommand::new("gh", arguments)
        .with_environment("CLICOLOR", "0")
        .with_environment("NO_COLOR", "1")
        .with_optional_working_directory(remote.command_working_directory.clone())
}

/// Parses one optional display id from a GitHub pull-request lookup response.
fn parse_lookup_display_id(stdout: &str) -> Result<Option<String>, String> {
    let pull_requests: Vec<GitHubLookupResponse> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub pull-request lookup response: {error}"))?;

    Ok(pull_requests
        .first()
        .map(|pull_request| format!("#{}", pull_request.number)))
}

/// Parses one pull-request summary from a `gh pr view` JSON response.
fn parse_view_response(stdout: &str) -> Result<ReviewRequestSummary, String> {
    let pull_request: GitHubViewResponse = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub pull-request view response: {error}"))?;
    let state = pull_request.review_request_state();
    let status_summary = pull_request.status_summary();

    Ok(ReviewRequestSummary {
        display_id: format!("#{}", pull_request.number),
        forge_kind: ForgeKind::GitHub,
        source_branch: pull_request.head_ref_name,
        state,
        status_summary,
        target_branch: pull_request.base_ref_name,
        title: pull_request.title,
        web_url: pull_request.url,
    })
}

/// Parses one GitHub pull-request display id into the numeric argument for
/// `gh`.
fn parse_display_id(display_id: &str) -> Result<String, ReviewRequestError> {
    let trimmed = display_id.trim().trim_start_matches('#');
    if trimmed.is_empty() || !trimmed.chars().all(|character| character.is_ascii_digit()) {
        return Err(ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: format!("invalid GitHub pull-request display id: `{display_id}`"),
        });
    }

    Ok(trimmed.to_string())
}

/// Formats one GitHub merge-state label for the UI.
fn merge_state_summary(merge_state_status: Option<&str>) -> Option<String> {
    match merge_state_status {
        Some("BLOCKED") => Some("Blocked".to_string()),
        Some("CLEAN") => Some("Mergeable".to_string()),
        Some("DIRTY") => Some("Conflicts".to_string()),
        Some("HAS_HOOKS") => Some("Hooks pending".to_string()),
        Some("UNSTABLE") => Some("Checks pending".to_string()),
        Some("UNKNOWN") | None => None,
        Some(other) => Some(normalize_provider_label(other)),
    }
}

/// Formats one GitHub review-decision label for the UI.
fn review_decision_summary(review_decision: Option<&str>) -> Option<String> {
    match review_decision {
        Some("APPROVED") => Some("Approved".to_string()),
        Some("CHANGES_REQUESTED") => Some("Changes requested".to_string()),
        Some("REVIEW_REQUIRED") => Some("Review required".to_string()),
        Some(other) => Some(normalize_provider_label(other)),
        None => None,
    }
}

/// GraphQL query text used to fetch review threads and review-request-wide
/// conversation comments for one pull request.
///
/// Capped at 100 threads per request and 100 comments per thread/PR.
const REVIEW_THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!) { repository(owner: $owner, name: \
     $repo) { pullRequest(number: $number) { comments(first: 100) { nodes { author { login } body \
     } } reviewThreads(first: 100) { nodes { isResolved path line comments(first: 100) { nodes { \
     author { login } body } } } } } } }";

/// Parses the full review-comment snapshot from a GraphQL response.
///
/// Threads are returned in the forge-reported order; callers sort by
/// `(path, line)` before rendering in the UI. PR-level conversation comments
/// preserve GitHub's chronological order.
fn parse_review_comment_snapshot_response(stdout: &str) -> Result<ReviewCommentSnapshot, String> {
    let response: GitHubReviewThreadsEnvelope = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub review-threads response: {error}"))?;

    let Some(data) = response.data else {
        return Err("GitHub review-threads response is missing a data payload".to_string());
    };
    let Some(pull_request) = data
        .repository
        .and_then(|repository| repository.pull_request)
    else {
        return Err("GitHub review-threads response is missing a pull request".to_string());
    };

    let threads = pull_request
        .review_threads
        .nodes
        .into_iter()
        .map(review_comment_thread_from_node)
        .collect();
    let pr_level_comments = pull_request
        .comments
        .map(|connection| {
            connection
                .nodes
                .into_iter()
                .map(review_comment_from_node)
                .collect()
        })
        .unwrap_or_default();

    Ok(ReviewCommentSnapshot {
        pr_level_comments,
        threads,
    })
}

/// Converts one GraphQL thread node into the forge-neutral representation.
fn review_comment_thread_from_node(node: GitHubReviewThreadNode) -> ReviewCommentThread {
    ReviewCommentThread {
        comments: node
            .comments
            .nodes
            .into_iter()
            .map(review_comment_from_node)
            .collect(),
        is_resolved: node.is_resolved,
        line: node.line,
        path: node.path,
    }
}

/// Converts one GraphQL comment node into the forge-neutral representation.
fn review_comment_from_node(node: GitHubReviewCommentNode) -> ReviewComment {
    ReviewComment {
        author: node
            .author
            .map_or_else(|| "ghost".to_string(), |author| author.login),
        body: node.body,
    }
}

/// Minimal GitHub API lookup payload used to find an existing pull request.
#[derive(Deserialize)]
struct GitHubLookupResponse {
    number: u64,
}

/// GraphQL response envelope for review-threads queries.
#[derive(Deserialize)]
struct GitHubReviewThreadsEnvelope {
    data: Option<GitHubReviewThreadsData>,
}

/// GraphQL `data` payload with the repository pull-request tree.
#[derive(Deserialize)]
struct GitHubReviewThreadsData {
    repository: Option<GitHubReviewThreadsRepository>,
}

/// GraphQL repository node carrying the pull-request field.
#[derive(Deserialize)]
struct GitHubReviewThreadsRepository {
    #[serde(rename = "pullRequest")]
    pull_request: Option<GitHubReviewThreadsPullRequest>,
}

/// GraphQL pull-request node carrying the review-threads connection and the
/// review-request-wide conversation comments.
#[derive(Deserialize)]
struct GitHubReviewThreadsPullRequest {
    comments: Option<GitHubReviewCommentsConnection>,
    #[serde(rename = "reviewThreads")]
    review_threads: GitHubReviewThreadsConnection,
}

/// GraphQL `reviewThreads` connection carrying the thread `nodes`.
#[derive(Deserialize)]
struct GitHubReviewThreadsConnection {
    nodes: Vec<GitHubReviewThreadNode>,
}

/// One GraphQL review-thread node.
#[derive(Deserialize)]
struct GitHubReviewThreadNode {
    comments: GitHubReviewCommentsConnection,
    #[serde(rename = "isResolved")]
    is_resolved: bool,
    line: Option<u32>,
    path: String,
}

/// GraphQL `comments` connection for one review thread.
#[derive(Deserialize)]
struct GitHubReviewCommentsConnection {
    nodes: Vec<GitHubReviewCommentNode>,
}

/// One GraphQL review-comment node.
#[derive(Deserialize)]
struct GitHubReviewCommentNode {
    author: Option<GitHubReviewCommentAuthor>,
    body: String,
}

/// GraphQL author node for a review comment. The `ghost` author is the only
/// case where `author` is `null` on GitHub today.
#[derive(Deserialize)]
struct GitHubReviewCommentAuthor {
    login: String,
}

/// GitHub pull-request JSON payload returned by `gh pr view --json`.
#[derive(Deserialize)]
struct GitHubViewResponse {
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
    #[serde(rename = "mergedAt")]
    merged_at: Option<String>,
    number: u64,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    state: String,
    title: String,
    url: String,
}

impl GitHubViewResponse {
    /// Maps GitHub state fields into the normalized review-request state.
    fn review_request_state(&self) -> ReviewRequestState {
        if self.merged_at.is_some() || self.state == "MERGED" {
            return ReviewRequestState::Merged;
        }

        if self.state == "CLOSED" {
            return ReviewRequestState::Closed;
        }

        ReviewRequestState::Open
    }

    /// Formats the provider-specific status summary for the UI.
    fn status_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.is_draft {
            parts.push("Draft".to_string());
        }

        if let Some(review_summary) = review_decision_summary(self.review_decision.as_deref()) {
            parts.push(review_summary);
        }

        if let Some(merge_summary) = merge_state_summary(self.merge_state_status.as_deref()) {
            parts.push(merge_summary);
        }

        status_summary_parts(&parts)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use mockall::Sequence;

    use super::*;
    use crate::command::MockForgeCommandRunner;

    #[tokio::test]
    async fn find_by_source_branch_builds_lookup_and_refresh_commands() {
        // Arrange
        let remote = github_remote();
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &auth_status_command(&remote)
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &lookup_command(&remote, "feature/forge")
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(r#"[{"number":42}]"#.to_string())) })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .find_by_source_branch(remote, "feature/forge".to_string())
            .await
            .expect("GitHub lookup should succeed");

        // Assert
        assert_eq!(
            review_request,
            Some(ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Approved, Mergeable".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn create_review_request_builds_create_command_and_returns_summary() {
        // Arrange
        let remote = github_remote();
        let input = CreateReviewRequestInput {
            body: Some("Implements the provider adapters.".to_string()),
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &auth_status_command(&remote)
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();
                let input = input.clone();

                move |command| command == &create_command(&remote, &input)
            })
            .returning(|_| {
                Box::pin(async {
                    Ok(success_output(
                        "https://github.com/agentty-xyz/agentty/pull/42\n".to_string(),
                    ))
                })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &lookup_command(&remote, "feature/forge")
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(r#"[{"number":42}]"#.to_string())) })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .create_review_request(remote, input)
            .await
            .expect("GitHub create should succeed");

        // Assert
        assert_eq!(review_request.display_id, "#42");
        assert_eq!(
            review_request.status_summary.as_deref(),
            Some("Approved, Mergeable")
        );
    }

    #[test]
    fn create_command_marks_pull_requests_as_draft_by_default() {
        // Arrange
        let remote = github_remote();
        let input = CreateReviewRequestInput {
            body: Some("Implements the provider adapters.".to_string()),
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };

        // Act
        let command = create_command(&remote, &input);

        // Assert
        assert_eq!(command.executable, "gh");
        assert!(
            command
                .arguments
                .iter()
                .any(|argument| argument == "--draft")
        );
    }

    #[test]
    fn github_commands_use_remote_working_directory_for_git_context() {
        // Arrange
        let remote =
            github_remote().with_command_working_directory(PathBuf::from("/tmp/session-worktree"));
        let input = CreateReviewRequestInput {
            body: Some("Implements the provider adapters.".to_string()),
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };

        // Act
        let auth_command = auth_status_command(&remote);
        let lookup_command = lookup_command(&remote, "feature/forge");
        let create_command = create_command(&remote, &input);
        let view_command = view_command(&remote, "42");

        // Assert
        assert_eq!(
            auth_command.working_directory,
            Some(PathBuf::from("/tmp/session-worktree"))
        );
        assert_eq!(
            lookup_command.working_directory,
            Some(PathBuf::from("/tmp/session-worktree"))
        );
        assert_eq!(
            create_command.working_directory,
            Some(PathBuf::from("/tmp/session-worktree"))
        );
        assert_eq!(
            view_command.working_directory,
            Some(PathBuf::from("/tmp/session-worktree"))
        );
    }

    #[tokio::test]
    async fn fetch_review_comment_snapshot_parses_graphql_response() {
        // Arrange
        let remote = github_remote();
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &auth_status_command(&remote)
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &review_threads_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_review_threads_json())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let snapshot = adapter
            .fetch_review_comment_snapshot(remote, "#42".to_string())
            .await
            .expect("GitHub review-comment snapshot fetch should succeed");

        // Assert
        assert_eq!(snapshot.threads.len(), 2);
        let unresolved = &snapshot.threads[0];
        assert_eq!(unresolved.path, "src/foo.rs");
        assert_eq!(unresolved.line, Some(42));
        assert!(!unresolved.is_resolved);
        assert_eq!(unresolved.comments.len(), 2);
        assert_eq!(unresolved.comments[0].author, "alice");
        assert_eq!(unresolved.comments[0].body, "Why aren't we handling None?");

        let resolved = &snapshot.threads[1];
        assert_eq!(resolved.path, "src/bar.rs");
        assert!(resolved.is_resolved);
        assert_eq!(resolved.comments.len(), 1);
        assert_eq!(resolved.comments[0].author, "ghost");

        assert_eq!(snapshot.pr_level_comments.len(), 2);
        assert_eq!(snapshot.pr_level_comments[0].author, "carol");
        assert_eq!(snapshot.pr_level_comments[0].body, "Overall looks good.");
        assert_eq!(snapshot.pr_level_comments[1].author, "ghost");
    }

    #[test]
    fn parse_review_comment_snapshot_response_rejects_missing_data() {
        // Arrange
        let stdout = "{\"data\": null}";

        // Act
        let error = parse_review_comment_snapshot_response(stdout)
            .expect_err("null data payload should be rejected");

        // Assert
        assert!(
            error.contains("missing a data payload"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parse_review_comment_snapshot_response_rejects_missing_pull_request() {
        // Arrange
        let stdout = "{\"data\": {\"repository\": {\"pullRequest\": null}}}";

        // Act
        let error = parse_review_comment_snapshot_response(stdout)
            .expect_err("null pull request should be rejected");

        // Assert
        assert!(
            error.contains("missing a pull request"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parse_review_comment_snapshot_response_returns_empty_snapshot_on_empty_threads() {
        // Arrange
        let stdout = r#"{"data": {"repository": {"pullRequest": {
            "reviewThreads": { "nodes": [] }
        }}}}"#;

        // Act
        let snapshot = parse_review_comment_snapshot_response(stdout)
            .expect("empty review thread list should parse");

        // Assert
        assert!(snapshot.threads.is_empty());
        assert!(snapshot.pr_level_comments.is_empty());
    }

    #[tokio::test]
    async fn refresh_review_request_maps_authentication_error() {
        // Arrange
        let remote = github_remote();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .withf({
                let remote = remote.clone();

                move |command| command == &auth_status_command(&remote)
            })
            .returning(|_| {
                Box::pin(async {
                    Ok(failure_output(
                        "You are not logged into any GitHub hosts. Run `gh auth login`."
                            .to_string(),
                    ))
                })
            });
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let error = adapter
            .refresh_review_request(remote, "#42".to_string())
            .await
            .expect_err("missing auth should be normalized");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::AuthenticationRequired {
                detail: Some(
                    "You are not logged into any GitHub hosts. Run `gh auth login`.".to_string()
                ),
                forge_kind: ForgeKind::GitHub,
                host: "github.com".to_string(),
            }
        );
    }

    fn github_remote() -> ForgeRemote {
        ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        }
    }

    fn github_review_threads_json() -> String {
        r#"{
            "data": {
                "repository": {
                    "pullRequest": {
                        "comments": {
                            "nodes": [
                                {
                                    "author": {"login": "carol"},
                                    "body": "Overall looks good."
                                },
                                {
                                    "author": null,
                                    "body": "Ghost conversation comment."
                                }
                            ]
                        },
                        "reviewThreads": {
                            "nodes": [
                                {
                                    "id": "thread-1",
                                    "isResolved": false,
                                    "isOutdated": false,
                                    "path": "src/foo.rs",
                                    "line": 42,
                                    "startLine": null,
                                    "diffSide": "RIGHT",
                                    "comments": {
                                        "nodes": [
                                            {
                                                "id": "comment-1",
                                                "author": {"login": "alice"},
                                                "body": "Why aren't we handling None?",
                                                "diffHunk": "@@ -40,3 +40,6 @@\n fn parse(input) {\n+    if raw.is_empty() {",
                                                "createdAt": "2026-04-19T10:00:00Z",
                                                "updatedAt": "2026-04-19T10:00:00Z",
                                                "url": "https://github.com/agentty-xyz/agentty/pull/42#discussion_r1"
                                            },
                                            {
                                                "id": "comment-2",
                                                "author": {"login": "bob"},
                                                "body": "Good catch. Will fix.",
                                                "diffHunk": "@@ -40,3 +40,6 @@\n fn parse(input) {\n+    if raw.is_empty() {",
                                                "createdAt": "2026-04-19T11:00:00Z",
                                                "updatedAt": "2026-04-19T11:00:00Z",
                                                "url": "https://github.com/agentty-xyz/agentty/pull/42#discussion_r2"
                                            }
                                        ]
                                    }
                                },
                                {
                                    "id": "thread-2",
                                    "isResolved": true,
                                    "isOutdated": false,
                                    "path": "src/bar.rs",
                                    "line": 15,
                                    "startLine": null,
                                    "diffSide": null,
                                    "comments": {
                                        "nodes": [
                                            {
                                                "id": "comment-3",
                                                "author": null,
                                                "body": "Resolved thread.",
                                                "diffHunk": "@@ -15 +15 @@\n-old\n+new",
                                                "createdAt": "2026-04-18T09:00:00Z",
                                                "updatedAt": "2026-04-18T09:00:00Z",
                                                "url": "https://github.com/agentty-xyz/agentty/pull/42#discussion_r3"
                                            }
                                        ]
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        }"#
        .to_string()
    }

    fn github_view_json() -> String {
        r#"{
            "number": 42,
            "title": "Add forge review support",
            "state": "OPEN",
            "url": "https://github.com/agentty-xyz/agentty/pull/42",
            "baseRefName": "main",
            "headRefName": "feature/forge",
            "isDraft": false,
            "mergeStateStatus": "CLEAN",
            "reviewDecision": "APPROVED",
            "mergedAt": null
        }"#
        .to_string()
    }

    fn success_output(stdout: String) -> ForgeCommandOutput {
        ForgeCommandOutput {
            exit_code: Some(0),
            stderr: String::new(),
            stdout,
        }
    }

    fn failure_output(stderr: String) -> ForgeCommandOutput {
        ForgeCommandOutput {
            exit_code: Some(1),
            stderr,
            stdout: String::new(),
        }
    }
}
