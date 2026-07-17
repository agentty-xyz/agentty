//! GitHub review-request adapter routed through the `gh` CLI.

use std::collections::HashSet;
use std::sync::Arc;

use serde::Deserialize;

use super::{
    AssignedIssue, CreateReviewRequestInput, ForgeCommand, ForgeCommandRunner, ForgeFuture,
    ForgeKind, ForgeRemote, IssueDetail, RequestedReview, RequestedReviewAudience, ReviewComment,
    ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread, ReviewRequestAdapter,
    ReviewRequestError, ReviewRequestOperations, ReviewRequestState, ReviewRequestSummary,
    SyncReviewRequestMetadataConfig, UpdateReviewRequestInput, map_parse_error,
    normalize_provider_label, operation_failed, parse_remote_url, status_summary_parts, strip_port,
};

/// Maximum requested-review rows loaded from `gh` for one refresh.
const REQUESTED_REVIEW_LIMIT: usize = 100;
/// Maximum assigned-issue rows loaded from `gh` for one refresh.
const ASSIGNED_ISSUE_LIMIT: usize = 100;
/// GraphQL query text used to fetch review threads and review-request-wide
/// conversation comments for one pull request.
///
/// Capped at 100 threads per request and 100 comments per thread/PR.
const REVIEW_THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!) { repository(owner: $owner, name: \
     $repo) { pullRequest(number: $number) { comments(first: 100) { nodes { author { login } body \
     } } reviewThreads(first: 100) { nodes { id diffSide isOutdated isResolved line path \
     startLine subjectType comments(first: 100) { nodes { author { login } body } } } } } } }";
/// GraphQL mutation used to add one reply to a pull-request review thread.
const REPLY_TO_THREAD_MUTATION: &str =
    "mutation($threadId: ID!, $body: String!) { addPullRequestReviewThreadReply(input: { \
     pullRequestReviewThreadId: $threadId, body: $body }) { comment { id } } }";
/// GraphQL mutation used to resolve one pull-request review thread.
const RESOLVE_THREAD_MUTATION: &str = "mutation($threadId: ID!) { resolveReviewThread(input: { \
                                       threadId: $threadId }) { thread { id isResolved } } }";

/// GitHub pull-request adapter that normalizes `gh` command output.
#[derive(Clone)]
pub(crate) struct GitHubReviewRequestAdapter {
    operations: ReviewRequestOperations,
}

impl GitHubReviewRequestAdapter {
    /// Builds one GitHub adapter from a forge command runner.
    pub(crate) fn new(command_runner: Arc<dyn ForgeCommandRunner>) -> Self {
        Self {
            operations: ReviewRequestOperations::new(command_runner),
        }
    }

    /// Returns normalized GitHub remote metadata when `repo_url` is supported.
    pub(crate) fn detect_remote(repo_url: &str) -> Option<ForgeRemote> {
        let parsed_remote = parse_remote_url(repo_url)?;
        if strip_port(&parsed_remote.host) != "github.com" {
            return None;
        }

        Some(parsed_remote.into_forge_remote(ForgeKind::GitHub))
    }

    /// Lists open GitHub issues assigned to the authenticated user in `remote`.
    pub(crate) fn list_assigned_issues(
        &self,
        remote: ForgeRemote,
    ) -> ForgeFuture<Result<Vec<AssignedIssue>, ReviewRequestError>> {
        let adapter = self.clone();

        Box::pin(async move {
            adapter.ensure_authenticated(&remote).await?;
            let output = adapter
                .operations
                .run_review_command(
                    &remote,
                    assigned_issues_command(&remote),
                    "list assigned issues",
                )
                .await?;

            map_parse_error(
                ForgeKind::GitHub,
                parse_assigned_issues_response(&output.stdout),
            )
        })
    }

    /// Fetches base details for one GitHub issue without requesting comments.
    pub(crate) fn fetch_issue_detail(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<IssueDetail, ReviewRequestError>> {
        let adapter = self.clone();

        Box::pin(async move {
            adapter.ensure_authenticated(&remote).await?;
            let output = adapter
                .operations
                .run_review_command(
                    &remote,
                    issue_detail_command(&remote, &display_id),
                    "load issue details",
                )
                .await?;

            map_parse_error(
                ForgeKind::GitHub,
                parse_issue_detail_response(&remote, &output.stdout),
            )
        })
    }
}

impl ReviewRequestAdapter for GitHubReviewRequestAdapter {
    fn ensure_authenticated(
        &self,
        remote: &ForgeRemote,
    ) -> ForgeFuture<Result<(), ReviewRequestError>> {
        self.operations
            .ensure_authenticated_future(remote.clone(), auth_status_command)
    }

    /// Finds one existing pull request for `source_branch`.
    fn find_authenticated_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>> {
        let adapter = self.clone();

        self.operations.find_by_source_branch_future(
            remote,
            source_branch,
            lookup_command,
            "find pull request",
            parse_lookup_display_id,
            move |remote, display_id| {
                adapter.refresh_authenticated_review_request(remote, display_id)
            },
        )
    }

    /// Creates one new draft pull request from `input`.
    fn create_authenticated_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        let adapter = self.clone();
        let operations = self.operations.clone();

        Box::pin(async move {
            let source_branch = input.source_branch.clone();
            let create_command = create_command(&remote, &input);
            operations
                .run_review_command(&remote, create_command, "create pull request")
                .await?;

            adapter
                .find_authenticated_by_source_branch(remote, source_branch)
                .await?
                .ok_or_else(|| {
                    operation_failed(
                        ForgeKind::GitHub,
                        "GitHub pull request was created but could not be reloaded",
                    )
                })
        })
    }

    /// Refreshes one existing pull request by display id.
    fn refresh_authenticated_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        self.operations.refresh_review_request_future(
            remote,
            display_id,
            parse_display_id,
            view_command,
            "refresh pull request",
            parse_view_response,
        )
    }

    /// Checks the current pull-request title/body and updates them when they
    /// differ from `input`.
    fn sync_authenticated_review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
        input: UpdateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        let adapter = self.clone();
        let config = SyncReviewRequestMetadataConfig {
            edit_metadata_command,
            edit_operation: "update pull-request metadata",
            parse_display_id,
            parse_metadata_response,
            requires_update: GitHubMetadataResponse::requires_update,
            view_metadata_command,
            view_operation: "view pull-request metadata",
        };

        self.operations.sync_review_request_metadata_future(
            remote,
            display_id,
            input,
            &config,
            move |remote, display_id| {
                adapter.refresh_authenticated_review_request(remote, display_id)
            },
        )
    }

    /// Fetches the review-comment snapshot for one existing pull request by
    /// display id through GitHub's GraphQL API.
    ///
    /// Returns both inline review threads anchored to diff lines and the
    /// review-request-wide "conversation" comments that are not anchored to a
    /// file or line.
    fn fetch_authenticated_review_comment_snapshot(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewCommentSnapshot, ReviewRequestError>> {
        self.operations.fetch_review_comment_snapshot_future(
            remote,
            display_id,
            parse_display_id,
            review_threads_command,
            "fetch review comments",
            parse_review_comment_snapshot_response,
        )
    }

    fn reply_to_authenticated_thread(
        &self,
        remote: ForgeRemote,
        display_id: String,
        thread_id: String,
        body: String,
    ) -> ForgeFuture<Result<(), ReviewRequestError>> {
        let operations = self.operations.clone();

        Box::pin(async move {
            parse_display_id(&display_id)?;
            operations
                .run_review_command(
                    &remote,
                    reply_to_thread_command(&remote, &thread_id, &body),
                    "reply to pull-request review thread",
                )
                .await?;

            Ok(())
        })
    }

    fn resolve_authenticated_thread(
        &self,
        remote: ForgeRemote,
        display_id: String,
        thread_id: String,
    ) -> ForgeFuture<Result<(), ReviewRequestError>> {
        let operations = self.operations.clone();

        Box::pin(async move {
            parse_display_id(&display_id)?;
            operations
                .run_review_command(
                    &remote,
                    resolve_thread_command(&remote, &thread_id),
                    "resolve pull-request review thread",
                )
                .await?;

            Ok(())
        })
    }

    /// Lists open pull requests in `remote` that request review from the
    /// current authenticated GitHub user, fetching the broader and direct
    /// requested-review searches concurrently after authentication.
    fn list_authenticated_requested_reviews(
        &self,
        remote: ForgeRemote,
    ) -> ForgeFuture<Result<Vec<RequestedReview>, ReviewRequestError>> {
        let operations = self.operations.clone();

        Box::pin(async move {
            let (all_output, personal_output) = tokio::try_join!(
                operations.run_review_command(
                    &remote,
                    requested_reviews_command(&remote),
                    "list requested pull-request reviews",
                ),
                operations.run_review_command(
                    &remote,
                    personal_requested_reviews_command(&remote),
                    "list personally requested pull-request reviews",
                )
            )?;

            let all_reviews = map_parse_error(
                remote.forge_kind,
                parse_requested_reviews_response(&all_output.stdout, &remote),
            )?;
            let personal_reviews = map_parse_error(
                remote.forge_kind,
                parse_requested_reviews_response(&personal_output.stdout, &remote),
            )?;

            Ok(categorize_requested_reviews(all_reviews, &personal_reviews))
        })
    }
}

/// Builds the project-scoped `gh search issues` command for assigned open
/// issues.
fn assigned_issues_command(remote: &ForgeRemote) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "search".to_string(),
            "issues".to_string(),
            "--assignee".to_string(),
            "@me".to_string(),
            "--state".to_string(),
            "open".to_string(),
            "--repo".to_string(),
            remote.project_path(),
            "--limit".to_string(),
            ASSIGNED_ISSUE_LIMIT.to_string(),
            "--json".to_string(),
            "number,title,url,updatedAt,repository".to_string(),
        ],
    )
}

/// Parses GitHub issue search rows into normalized assigned-issue rows.
fn parse_assigned_issues_response(stdout: &str) -> Result<Vec<AssignedIssue>, String> {
    let issues: Vec<GitHubAssignedIssueResponse> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub assigned-issue response: {error}"))?;

    Ok(issues
        .into_iter()
        .map(|issue| AssignedIssue {
            display_id: format!("#{}", issue.number),
            repository: issue.repository.name_with_owner,
            title: issue.title,
            updated_at: issue.updated_at,
            web_url: issue.url,
        })
        .collect())
}

/// Builds the project-scoped `gh issue view` command for base issue details.
fn issue_detail_command(remote: &ForgeRemote, display_id: &str) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "issue".to_string(),
            "view".to_string(),
            display_id.trim_start_matches('#').to_string(),
            "--repo".to_string(),
            remote.project_path(),
            "--json".to_string(),
            "assignees,author,body,createdAt,labels,number,state,title,updatedAt,url".to_string(),
        ],
    )
}

/// Parses one GitHub issue detail response without comment data.
fn parse_issue_detail_response(remote: &ForgeRemote, stdout: &str) -> Result<IssueDetail, String> {
    let issue: GitHubIssueDetailResponse = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub issue-detail response: {error}"))?;

    Ok(IssueDetail {
        assignees: issue.assignees.into_iter().map(|user| user.login).collect(),
        author: issue
            .author
            .map_or_else(|| "ghost".to_string(), |author| author.login),
        body: issue.body,
        created_at: issue.created_at,
        display_id: format!("#{}", issue.number),
        labels: issue.labels.into_iter().map(|label| label.name).collect(),
        repository: remote.project_path(),
        state: issue.state,
        title: issue.title,
        updated_at: issue.updated_at,
        web_url: issue.url,
    })
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

/// Builds the `gh api` lookup command for open pull requests matching
/// `source_branch`.
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
            "state=open".to_string(),
            "-f".to_string(),
            "sort=created".to_string(),
            "-f".to_string(),
            "direction=desc".to_string(),
            "-f".to_string(),
            "per_page=1".to_string(),
        ],
    )
}

/// Parses one optional display id from a GitHub pull-request lookup response.
fn parse_lookup_display_id(stdout: &str) -> Result<Option<String>, String> {
    let pull_requests: Vec<GitHubLookupResponse> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub pull-request lookup response: {error}"))?;

    Ok(pull_requests
        .first()
        .map(|pull_request| format!("#{}", pull_request.number)))
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

/// Builds the `gh pr view` command that reads title/body metadata used for
/// change detection before editing a pull request.
fn view_metadata_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "pr".to_string(),
            "view".to_string(),
            pull_request_number.to_string(),
            "--repo".to_string(),
            remote.project_path(),
            "--json".to_string(),
            "title,body".to_string(),
        ],
    )
}

/// Parses current pull-request title/body metadata from `gh pr view` JSON.
fn parse_metadata_response(stdout: &str) -> Result<GitHubMetadataResponse, String> {
    serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub pull-request metadata response: {error}"))
}

/// Builds the `gh pr edit` command for updating one pull-request title/body.
fn edit_metadata_command(
    remote: &ForgeRemote,
    pull_request_number: &str,
    input: &UpdateReviewRequestInput,
) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "pr".to_string(),
            "edit".to_string(),
            pull_request_number.to_string(),
            "--repo".to_string(),
            remote.project_path(),
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

/// Builds one `gh api graphql` mutation that replies to a review thread.
fn reply_to_thread_command(remote: &ForgeRemote, thread_id: &str, body: &str) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "api".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
            "graphql".to_string(),
            "-f".to_string(),
            format!("query={REPLY_TO_THREAD_MUTATION}"),
            "-f".to_string(),
            format!("threadId={thread_id}"),
            "-f".to_string(),
            format!("body={body}"),
        ],
    )
}

/// Builds one `gh api graphql` mutation that resolves a review thread.
fn resolve_thread_command(remote: &ForgeRemote, thread_id: &str) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "api".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
            "graphql".to_string(),
            "-f".to_string(),
            format!("query={RESOLVE_THREAD_MUTATION}"),
            "-f".to_string(),
            format!("threadId={thread_id}"),
        ],
    )
}

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
    let line = if node.subject_type == "FILE" {
        None
    } else {
        node.line
    };
    ReviewCommentThread {
        anchor_side: github_anchor_side(&node),
        comments: node
            .comments
            .nodes
            .into_iter()
            .map(review_comment_from_node)
            .collect(),
        id: node.id,
        is_outdated: Some(node.is_outdated),
        is_resolved: node.is_resolved,
        line,
        path: node.path,
        start_line: node.start_line,
    }
}

/// Converts GitHub's diff-side labels into Agentty's normalized anchor side.
fn github_anchor_side(node: &GitHubReviewThreadNode) -> ReviewCommentAnchorSide {
    if node.subject_type == "FILE" || node.line.is_none() {
        return ReviewCommentAnchorSide::File;
    }

    match node.diff_side.as_str() {
        "LEFT" => ReviewCommentAnchorSide::Old,
        _ => ReviewCommentAnchorSide::New,
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

/// Builds the `gh search prs` command for PRs requesting the current user's
/// review in the selected repository.
fn requested_reviews_command(remote: &ForgeRemote) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "search".to_string(),
            "prs".to_string(),
            "--review-requested".to_string(),
            "@me".to_string(),
            "--state".to_string(),
            "open".to_string(),
            "--repo".to_string(),
            remote.project_path(),
            "--limit".to_string(),
            REQUESTED_REVIEW_LIMIT.to_string(),
            "--json".to_string(),
            "number,title,body,url,isDraft,updatedAt,author".to_string(),
        ],
    )
}

/// Builds the `gh search prs` command for pull requests requesting review
/// from the current GitHub user directly, excluding team-only requests.
fn personal_requested_reviews_command(remote: &ForgeRemote) -> ForgeCommand {
    github_command(
        remote,
        vec![
            "search".to_string(),
            "prs".to_string(),
            "user-review-requested:@me".to_string(),
            "--state".to_string(),
            "open".to_string(),
            "--repo".to_string(),
            remote.project_path(),
            "--limit".to_string(),
            REQUESTED_REVIEW_LIMIT.to_string(),
            "--json".to_string(),
            "number,title,body,url,isDraft,updatedAt,author".to_string(),
        ],
    )
}

/// Parses GitHub search rows into normalized requested-review rows.
fn parse_requested_reviews_response(
    stdout: &str,
    remote: &ForgeRemote,
) -> Result<Vec<RequestedReview>, String> {
    let pull_requests: Vec<GitHubRequestedReviewResponse> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub requested-review response: {error}"))?;

    Ok(pull_requests
        .into_iter()
        .map(|pull_request| {
            let status_summary = if pull_request.is_draft {
                Some("Draft".to_string())
            } else {
                None
            };

            RequestedReview {
                audience: RequestedReviewAudience::Personal,
                author: pull_request
                    .author
                    .map_or_else(|| "ghost".to_string(), |author| author.login),
                body: pull_request.body,
                comment_snapshot: None,
                display_id: format!("#{}", pull_request.number),
                forge_kind: ForgeKind::GitHub,
                repository: remote.project_path(),
                status_summary,
                title: pull_request.title,
                updated_at: pull_request.updated_at,
                web_url: pull_request.url,
            }
        })
        .collect())
}

/// Merges requested-review rows from both GitHub searches, marking rows as
/// personal when they appear in the `user-review-requested:@me` result and as
/// group requests otherwise.
///
/// Rows from the broader search keep their original order, while personal-only
/// rows are appended so brief API timing or pagination differences do not drop
/// directly requested pull requests from the UI.
fn categorize_requested_reviews(
    all_reviews: Vec<RequestedReview>,
    personal_reviews: &[RequestedReview],
) -> Vec<RequestedReview> {
    let personal_urls = personal_reviews
        .iter()
        .map(|review| review.web_url.as_str())
        .collect::<HashSet<_>>();
    let mut seen_urls = HashSet::new();
    let mut categorized_reviews = Vec::with_capacity(all_reviews.len().max(personal_reviews.len()));

    for mut review in all_reviews {
        review.audience = if personal_urls.contains(review.web_url.as_str()) {
            RequestedReviewAudience::Personal
        } else {
            RequestedReviewAudience::Group
        };

        if seen_urls.insert(review.web_url.clone()) {
            categorized_reviews.push(review);
        }
    }

    for review in personal_reviews {
        if seen_urls.insert(review.web_url.clone()) {
            let mut review = review.clone();
            review.audience = RequestedReviewAudience::Personal;
            categorized_reviews.push(review);
        }
    }

    categorized_reviews
}

/// Builds one base `gh` command with deterministic color settings and the
/// optional session worktree for repository-aware git fallback commands.
fn github_command(remote: &ForgeRemote, arguments: Vec<String>) -> ForgeCommand {
    ForgeCommand::new("gh", arguments)
        .with_environment("CLICOLOR", "0")
        .with_environment("NO_COLOR", "1")
        .with_optional_working_directory(remote.command_working_directory.clone())
}

/// Minimal GitHub API lookup payload used to find an existing pull request.
#[derive(Deserialize)]
struct GitHubLookupResponse {
    number: u64,
}

/// GitHub search row returned by `gh search prs --json`.
#[derive(Deserialize)]
struct GitHubRequestedReviewResponse {
    author: Option<GitHubRequestedReviewAuthor>,
    #[serde(default)]
    body: Option<String>,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    number: u64,
    title: String,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
    url: String,
}

/// GitHub search author node for the user who opened a requested review.
#[derive(Deserialize)]
struct GitHubRequestedReviewAuthor {
    login: String,
}

/// GitHub search row returned by `gh search issues --json`.
#[derive(Deserialize)]
struct GitHubAssignedIssueResponse {
    number: u64,
    repository: GitHubAssignedIssueRepository,
    title: String,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
    url: String,
}

/// Repository identity nested in one GitHub issue search row.
#[derive(Deserialize)]
struct GitHubAssignedIssueRepository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

/// GitHub issue-detail payload returned by `gh issue view --json`.
#[derive(Deserialize)]
struct GitHubIssueDetailResponse {
    assignees: Vec<GitHubIssueUserResponse>,
    author: Option<GitHubIssueUserResponse>,
    body: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
    labels: Vec<GitHubIssueLabelResponse>,
    number: u64,
    state: String,
    title: String,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
    url: String,
}

/// GitHub user identity nested in an issue-detail payload.
#[derive(Deserialize)]
struct GitHubIssueUserResponse {
    login: String,
}

/// GitHub label identity nested in an issue-detail payload.
#[derive(Deserialize)]
struct GitHubIssueLabelResponse {
    name: String,
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
    #[serde(rename = "diffSide")]
    diff_side: String,
    #[serde(rename = "isOutdated")]
    is_outdated: bool,
    #[serde(rename = "isResolved")]
    is_resolved: bool,
    id: String,
    line: Option<u32>,
    path: String,
    #[serde(rename = "startLine")]
    start_line: Option<u32>,
    #[serde(rename = "subjectType")]
    subject_type: String,
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

        if let Some(review_summary) = Self::review_decision_summary(self.review_decision.as_deref())
        {
            parts.push(review_summary);
        }

        if let Some(merge_summary) = Self::merge_state_summary(self.merge_state_status.as_deref()) {
            parts.push(merge_summary);
        }

        status_summary_parts(&parts)
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
}

/// GitHub pull-request title/body payload returned by `gh pr view --json`.
#[derive(Deserialize)]
struct GitHubMetadataResponse {
    #[serde(default)]
    body: String,
    title: String,
}

impl GitHubMetadataResponse {
    /// Returns whether the remote metadata differs from the desired input.
    fn requires_update(&self, input: &UpdateReviewRequestInput) -> bool {
        self.title != input.title || self.body != input.body.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use mockall::Sequence;

    use super::*;
    use crate::command::{ForgeCommandOutput, MockForgeCommandRunner};

    #[tokio::test]
    async fn list_assigned_issues_authenticates_and_normalizes_rows() {
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

                move |command| command == &assigned_issues_command(&remote)
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_assigned_issues_json())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let issues = adapter
            .list_assigned_issues(remote)
            .await
            .expect("assigned issue search should succeed");

        // Assert
        assert_eq!(
            issues,
            vec![AssignedIssue {
                display_id: "#124".to_string(),
                repository: "agentty-xyz/agentty".to_string(),
                title: "Keep issue list compact".to_string(),
                updated_at: Some("2026-07-09T18:30:00Z".to_string()),
                web_url: "https://github.com/agentty-xyz/agentty/issues/124".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn fetch_issue_detail_omits_comments_and_normalizes_base_fields() {
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

                move |command| command == &issue_detail_command(&remote, "#124")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_issue_detail_json())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let detail = adapter
            .fetch_issue_detail(remote, "#124".to_string())
            .await
            .expect("issue detail query should succeed");

        // Assert
        assert_eq!(
            detail,
            IssueDetail {
                assignees: vec!["octocat".to_string()],
                author: "hubot".to_string(),
                body: Some("Issue details without comments.".to_string()),
                created_at: Some("2026-07-01T10:00:00Z".to_string()),
                display_id: "#124".to_string(),
                labels: vec!["enhancement".to_string(), "ui".to_string()],
                repository: "agentty-xyz/agentty".to_string(),
                state: "OPEN".to_string(),
                title: "Keep issue list compact".to_string(),
                updated_at: Some("2026-07-09T18:30:00Z".to_string()),
                web_url: "https://github.com/agentty-xyz/agentty/issues/124".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn find_authenticated_by_source_branch_builds_lookup_and_refresh_commands() {
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
            .find_authenticated_by_source_branch(remote, "feature/forge".to_string())
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

    #[test]
    fn lookup_command_limits_lookup_to_open_pull_requests() {
        // Arrange
        let remote = github_remote();

        // Act
        let command = lookup_command(&remote, "feature/forge");

        // Assert
        assert!(command.arguments.contains(&"state=open".to_string()));
        assert!(!command.arguments.contains(&"state=all".to_string()));
    }

    #[tokio::test]
    async fn create_authenticated_review_request_builds_create_command_and_returns_summary() {
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
            .create_authenticated_review_request(remote, input)
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
    fn parse_display_id_rejects_invalid_pull_request_reference() {
        // Arrange
        let display_id = "#not-a-number";

        // Act
        let error = parse_display_id(display_id).expect_err("invalid display id should fail");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message: "invalid GitHub pull-request display id: `#not-a-number`".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn sync_authenticated_review_request_metadata_edits_changed_pull_request() {
        // Arrange
        let remote = github_remote();
        let input = UpdateReviewRequestInput {
            body: Some("Updated body.".to_string()),
            title: "Refine forge review support".to_string(),
        };
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_metadata_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_metadata_json())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();
                let input = input.clone();

                move |command| command == &edit_metadata_command(&remote, "42", &input)
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
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
            .sync_authenticated_review_request_metadata(remote, "#42".to_string(), input)
            .await
            .expect("GitHub metadata sync should succeed");

        // Assert
        assert_eq!(review_request.display_id, "#42");
    }

    #[tokio::test]
    async fn sync_authenticated_review_request_metadata_skips_edit_when_unchanged() {
        // Arrange
        let remote = github_remote();
        let input = UpdateReviewRequestInput {
            body: Some("Current body.".to_string()),
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

                move |command| command == &view_metadata_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_metadata_json())) }));
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
            .sync_authenticated_review_request_metadata(remote, "#42".to_string(), input)
            .await
            .expect("GitHub metadata sync should succeed");

        // Assert
        assert_eq!(review_request.display_id, "#42");
    }

    #[tokio::test]
    async fn list_authenticated_requested_reviews_separates_personal_and_group_rows() {
        // Arrange
        let remote = github_remote();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .withf({
                let remote = remote.clone();

                move |command| command == &requested_reviews_command(&remote)
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_requested_reviews_json())) }));
        command_runner
            .expect_run()
            .once()
            .withf({
                let remote = remote.clone();

                move |command| command == &personal_requested_reviews_command(&remote)
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(github_personal_requested_reviews_json())) })
            });
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let requested_reviews = adapter
            .list_authenticated_requested_reviews(remote)
            .await
            .expect("GitHub requested reviews should load");

        // Assert
        assert_eq!(
            requested_reviews,
            vec![
                RequestedReview {
                    audience: RequestedReviewAudience::Personal,
                    author: "octocat".to_string(),
                    body: Some("Implements the GitHub provider.".to_string()),
                    comment_snapshot: None,
                    display_id: "#42".to_string(),
                    forge_kind: ForgeKind::GitHub,
                    repository: "agentty-xyz/agentty".to_string(),
                    status_summary: Some("Draft".to_string()),
                    title: "Add forge review support".to_string(),
                    updated_at: Some("2026-04-27T21:30:00Z".to_string()),
                    web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                },
                RequestedReview {
                    audience: RequestedReviewAudience::Group,
                    author: "team-lead".to_string(),
                    body: Some("Adds team-owned parser coverage.".to_string()),
                    comment_snapshot: None,
                    display_id: "#43".to_string(),
                    forge_kind: ForgeKind::GitHub,
                    repository: "agentty-xyz/agentty".to_string(),
                    status_summary: None,
                    title: "Review team-owned parser".to_string(),
                    updated_at: Some("2026-04-28T21:30:00Z".to_string()),
                    web_url: "https://github.com/agentty-xyz/agentty/pull/43".to_string(),
                },
                RequestedReview {
                    audience: RequestedReviewAudience::Personal,
                    author: "reviewer".to_string(),
                    body: Some("Adds direct-only parser coverage.".to_string()),
                    comment_snapshot: None,
                    display_id: "#44".to_string(),
                    forge_kind: ForgeKind::GitHub,
                    repository: "agentty-xyz/agentty".to_string(),
                    status_summary: None,
                    title: "Review direct-only parser".to_string(),
                    updated_at: Some("2026-04-29T21:30:00Z".to_string()),
                    web_url: "https://github.com/agentty-xyz/agentty/pull/44".to_string(),
                },
            ]
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
    async fn fetch_authenticated_review_comment_snapshot_parses_graphql_response() {
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

                move |command| command == &review_threads_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_review_threads_json())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let snapshot = adapter
            .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
            .await
            .expect("GitHub review-comment snapshot fetch should succeed");

        // Assert
        assert_eq!(snapshot.threads.len(), 3);
        let unresolved = &snapshot.threads[0];
        assert_eq!(unresolved.id, "thread-1");
        assert_eq!(unresolved.path, "src/foo.rs");
        assert_eq!(unresolved.line, Some(42));
        assert_eq!(unresolved.anchor_side, ReviewCommentAnchorSide::New);
        assert_eq!(unresolved.is_outdated, Some(false));
        assert!(!unresolved.is_resolved);
        assert_eq!(unresolved.comments.len(), 2);
        assert_eq!(unresolved.comments[0].author, "alice");
        assert_eq!(unresolved.comments[0].body, "Why aren't we handling None?");

        let resolved = &snapshot.threads[1];
        assert_eq!(resolved.path, "src/bar.rs");
        assert_eq!(resolved.anchor_side, ReviewCommentAnchorSide::Old);
        assert!(resolved.is_resolved);
        assert_eq!(resolved.comments.len(), 1);
        assert_eq!(resolved.comments[0].author, "ghost");

        let file_level = &snapshot.threads[2];
        assert_eq!(file_level.path, "Cargo.toml");
        assert_eq!(file_level.line, None);
        assert_eq!(file_level.anchor_side, ReviewCommentAnchorSide::File);

        assert_eq!(snapshot.pr_level_comments.len(), 2);
        assert_eq!(snapshot.pr_level_comments[0].author, "carol");
        assert_eq!(snapshot.pr_level_comments[0].body, "Overall looks good.");
        assert_eq!(snapshot.pr_level_comments[1].author, "ghost");
    }

    #[tokio::test]
    async fn review_thread_reply_and_resolution_run_graphql_mutations() {
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

                move |command| {
                    command == &reply_to_thread_command(&remote, "thread-1", "Addressed.")
                }
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &resolve_thread_command(&remote, "thread-1")
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let reply_result = adapter
            .reply_to_authenticated_thread(
                remote.clone(),
                "#42".to_string(),
                "thread-1".to_string(),
                "Addressed.".to_string(),
            )
            .await;
        let resolution_result = adapter
            .resolve_authenticated_thread(remote, "#42".to_string(), "thread-1".to_string())
            .await;

        // Assert
        assert_eq!(reply_result, Ok(()));
        assert_eq!(resolution_result, Ok(()));
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
    async fn refresh_authenticated_review_request_maps_authentication_error() {
        // Arrange
        let remote = github_remote();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
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
            .refresh_authenticated_review_request(remote, "#42".to_string())
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

    #[test]
    fn github_status_helpers_map_provider_labels() {
        // Arrange
        let review_decision_cases = [
            (Some("APPROVED"), Some("Approved")),
            (Some("CHANGES_REQUESTED"), Some("Changes requested")),
            (Some("REVIEW_REQUIRED"), Some("Review required")),
            (Some("COMMENTED"), Some("Commented")),
            (None, None),
        ];
        let merge_state_cases = [
            (Some("BLOCKED"), Some("Blocked")),
            (Some("CLEAN"), Some("Mergeable")),
            (Some("DIRTY"), Some("Conflicts")),
            (Some("HAS_HOOKS"), Some("Hooks pending")),
            (Some("UNSTABLE"), Some("Checks pending")),
            (Some("UNKNOWN"), None),
            (Some("BEHIND"), Some("Behind")),
            (None, None),
        ];

        // Act & Assert
        for (status, expected) in review_decision_cases {
            assert_eq!(
                GitHubViewResponse::review_decision_summary(status).as_deref(),
                expected
            );
        }
        for (status, expected) in merge_state_cases {
            assert_eq!(
                GitHubViewResponse::merge_state_summary(status).as_deref(),
                expected
            );
        }
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
                                    "subjectType": "LINE",
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
                                    "diffSide": "LEFT",
                                    "subjectType": "LINE",
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
                                },
                                {
                                    "id": "thread-3",
                                    "isResolved": false,
                                    "isOutdated": false,
                                    "path": "Cargo.toml",
                                    "line": null,
                                    "startLine": null,
                                    "diffSide": "RIGHT",
                                    "subjectType": "FILE",
                                    "comments": {
                                        "nodes": []
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

    fn github_metadata_json() -> String {
        r#"{
            "body": "Current body.",
            "title": "Add forge review support"
        }"#
        .to_string()
    }

    fn github_assigned_issues_json() -> String {
        r#"[
            {
                "number": 124,
                "repository": {"nameWithOwner": "agentty-xyz/agentty"},
                "title": "Keep issue list compact",
                "updatedAt": "2026-07-09T18:30:00Z",
                "url": "https://github.com/agentty-xyz/agentty/issues/124"
            }
        ]"#
        .to_string()
    }

    fn github_issue_detail_json() -> String {
        r#"{
            "assignees": [{"login": "octocat"}],
            "author": {"login": "hubot"},
            "body": "Issue details without comments.",
            "createdAt": "2026-07-01T10:00:00Z",
            "labels": [{"name": "enhancement"}, {"name": "ui"}],
            "number": 124,
            "state": "OPEN",
            "title": "Keep issue list compact",
            "updatedAt": "2026-07-09T18:30:00Z",
            "url": "https://github.com/agentty-xyz/agentty/issues/124"
        }"#
        .to_string()
    }

    /// Returns one `gh search prs --json` fixture for requested reviews.
    fn github_requested_reviews_json() -> String {
        r#"[
            {
                "isDraft": true,
                "number": 42,
                "title": "Add forge review support",
                "author": {"login": "octocat"},
                "body": "Implements the GitHub provider.",
                "updatedAt": "2026-04-27T21:30:00Z",
                "url": "https://github.com/agentty-xyz/agentty/pull/42"
            },
            {
                "isDraft": false,
                "number": 43,
                "title": "Review team-owned parser",
                "author": {"login": "team-lead"},
                "body": "Adds team-owned parser coverage.",
                "updatedAt": "2026-04-28T21:30:00Z",
                "url": "https://github.com/agentty-xyz/agentty/pull/43"
            }
        ]"#
        .to_string()
    }

    /// Returns one `user-review-requested:@me` fixture for personal reviews.
    fn github_personal_requested_reviews_json() -> String {
        r#"[
            {
                "isDraft": true,
                "number": 42,
                "title": "Add forge review support",
                "author": {"login": "octocat"},
                "body": "Implements the GitHub provider.",
                "updatedAt": "2026-04-27T21:30:00Z",
                "url": "https://github.com/agentty-xyz/agentty/pull/42"
            },
            {
                "isDraft": false,
                "number": 44,
                "title": "Review direct-only parser",
                "author": {"login": "reviewer"},
                "body": "Adds direct-only parser coverage.",
                "updatedAt": "2026-04-29T21:30:00Z",
                "url": "https://github.com/agentty-xyz/agentty/pull/44"
            }
        ]"#
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
