//! GitHub review-request adapter routed through the `gh` CLI.

use std::collections::HashSet;
use std::sync::Arc;

use serde::Deserialize;

use super::{
    CreateReviewRequestInput, ForgeCommand, ForgeCommandRunner, ForgeFuture, ForgeKind,
    ForgeRemote, RequestedReview, RequestedReviewAudience, ReviewComment, ReviewCommentAnchorSide,
    ReviewCommentSnapshot, ReviewCommentThread, ReviewRequestAdapter, ReviewRequestError,
    ReviewRequestMetadata, ReviewRequestMetadataEdit, ReviewRequestOperations, ReviewRequestState,
    ReviewRequestSummary, SyncReviewRequestMetadataConfig, UpdateReviewRequestInput,
    map_parse_error, normalize_provider_label, operation_failed, parse_remote_url,
    status_summary_parts, strip_port,
};

/// Maximum requested-review rows loaded from `gh` for one refresh.
const REQUESTED_REVIEW_LIMIT: usize = 100;
/// Paginated GraphQL query used to fetch review threads for one pull request.
const REVIEW_THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) { \
     repository(owner: $owner, name: $repo) { pullRequest(number: $number) { reviewThreads(first: \
     100, after: $endCursor) { nodes { id diffSide isOutdated isResolved line path startLine \
     subjectType comments(first: 100) { nodes { author { login } body } pageInfo { hasNextPage \
     endCursor } } } pageInfo { hasNextPage endCursor } } } } }";
/// Paginated GraphQL query used to fetch pull-request conversation comments.
const PULL_REQUEST_COMMENTS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) { \
     repository(owner: $owner, name: $repo) { pullRequest(number: $number) { comments(first: 100, \
     after: $endCursor) { nodes { author { login } body } pageInfo { hasNextPage endCursor } } } \
     } }";
/// Paginated GraphQL query used when one review thread exceeds 100 comments.
const THREAD_COMMENTS_QUERY: &str = "query($threadId: ID!, $endCursor: String) { node(id: \
                                     $threadId) { ... on PullRequestReviewThread { \
                                     comments(first: 100, after: $endCursor) { nodes { author { \
                                     login } body } pageInfo { hasNextPage endCursor } } } } }";
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

    /// Loads current pull-request title/body metadata.
    fn authenticated_review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestMetadata, ReviewRequestError>> {
        self.operations
            .review_request_metadata_future(remote, display_id, &metadata_sync_config())
    }

    /// Checks the current pull-request title/body and updates fields that still
    /// match the reconciled input.
    fn sync_authenticated_review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
        input: UpdateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        let adapter = self.clone();

        self.operations.sync_review_request_metadata_future(
            remote,
            display_id,
            input,
            &metadata_sync_config(),
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
        let operations = self.operations.clone();

        Box::pin(async move {
            let pull_request_number = parse_display_id(&display_id)?;
            let threads_output = operations
                .run_review_command(
                    &remote,
                    review_threads_command(&remote, &pull_request_number),
                    "fetch pull-request review threads",
                )
                .await?;
            let mut thread_nodes = map_parse_error(
                ForgeKind::GitHub,
                parse_review_thread_pages(&threads_output.stdout),
            )?;
            let comments_output = operations
                .run_review_command(
                    &remote,
                    pull_request_comments_command(&remote, &pull_request_number),
                    "fetch pull-request conversation comments",
                )
                .await?;
            let pr_level_comments = map_parse_error(
                ForgeKind::GitHub,
                parse_pull_request_comment_pages(&comments_output.stdout),
            )?;

            for thread_node in &mut thread_nodes {
                if !thread_node.comments.page_info.has_next_page {
                    continue;
                }

                let comments_output = operations
                    .run_review_command(
                        &remote,
                        thread_comments_command(&remote, &thread_node.id),
                        "fetch review-thread comments",
                    )
                    .await?;
                thread_node.comments.nodes = map_parse_error(
                    ForgeKind::GitHub,
                    parse_thread_comment_pages(&comments_output.stdout),
                )?;
            }

            Ok(ReviewCommentSnapshot {
                pr_level_comments,
                threads: thread_nodes
                    .into_iter()
                    .map(review_comment_thread_from_node)
                    .collect(),
            })
        })
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

/// Builds GitHub-specific metadata view and edit configuration.
fn metadata_sync_config() -> SyncReviewRequestMetadataConfig {
    SyncReviewRequestMetadataConfig {
        edit_metadata_command,
        edit_operation: "update pull-request metadata",
        parse_display_id,
        parse_metadata_response,
        view_metadata_command,
        view_operation: "view pull-request metadata",
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
fn parse_metadata_response(stdout: &str) -> Result<ReviewRequestMetadata, String> {
    let metadata: GitHubMetadataResponse = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub pull-request metadata response: {error}"))?;

    Ok(ReviewRequestMetadata {
        body: metadata.body,
        title: metadata.title,
    })
}

/// Builds the `gh pr edit` command for updating one pull-request title/body.
fn edit_metadata_command(
    remote: &ForgeRemote,
    pull_request_number: &str,
    edit: &ReviewRequestMetadataEdit,
) -> ForgeCommand {
    let mut arguments = vec![
        "pr".to_string(),
        "edit".to_string(),
        pull_request_number.to_string(),
        "--repo".to_string(),
        remote.project_path(),
    ];
    if let Some(title) = edit.title.as_ref() {
        arguments.extend(["--title".to_string(), title.clone()]);
    }
    if let Some(body) = edit.body.as_ref() {
        arguments.extend(["--body".to_string(), body.clone()]);
    }

    github_command(remote, arguments)
}

/// Builds one paginated `gh api graphql` command that fetches review threads.
fn review_threads_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
    paginated_graphql_command(
        remote,
        REVIEW_THREADS_QUERY,
        vec![
            format!("owner={}", remote.namespace),
            format!("repo={}", remote.project),
            format!("number={pull_request_number}"),
        ],
    )
}

/// Builds one paginated GraphQL command for pull-request conversation comments.
fn pull_request_comments_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
    paginated_graphql_command(
        remote,
        PULL_REQUEST_COMMENTS_QUERY,
        vec![
            format!("owner={}", remote.namespace),
            format!("repo={}", remote.project),
            format!("number={pull_request_number}"),
        ],
    )
}

/// Builds one paginated GraphQL command for all comments in one review thread.
fn thread_comments_command(remote: &ForgeRemote, thread_id: &str) -> ForgeCommand {
    paginated_graphql_command(
        remote,
        THREAD_COMMENTS_QUERY,
        vec![format!("threadId={thread_id}")],
    )
}

/// Builds one `gh api graphql --paginate --slurp` command.
fn paginated_graphql_command(
    remote: &ForgeRemote,
    query: &str,
    variables: Vec<String>,
) -> ForgeCommand {
    let mut arguments = vec![
        "api".to_string(),
        "--hostname".to_string(),
        remote.host.clone(),
        "graphql".to_string(),
        "--paginate".to_string(),
        "--slurp".to_string(),
        "-f".to_string(),
        format!("query={query}"),
    ];
    for variable in variables {
        arguments.extend(["-F".to_string(), variable]);
    }

    github_command(remote, arguments)
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

/// Parses and combines all `--slurp` pages from a review-threads query.
fn parse_review_thread_pages(stdout: &str) -> Result<Vec<GitHubReviewThreadNode>, String> {
    let responses: Vec<GitHubReviewThreadsEnvelope> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub review-threads response: {error}"))?;
    let mut thread_nodes = Vec::new();
    for response in responses {
        let Some(data) = response.data else {
            return Err("GitHub review-threads response is missing a data payload".to_string());
        };
        let Some(pull_request) = data
            .repository
            .and_then(|repository| repository.pull_request)
        else {
            return Err("GitHub review-threads response is missing a pull request".to_string());
        };

        thread_nodes.extend(pull_request.review_threads.nodes);
    }

    Ok(thread_nodes)
}

/// Parses and combines all pull-request conversation-comment pages.
fn parse_pull_request_comment_pages(stdout: &str) -> Result<Vec<ReviewComment>, String> {
    let responses: Vec<GitHubPullRequestCommentsEnvelope> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub pull-request comments response: {error}"))?;
    let mut comments = Vec::new();
    for response in responses {
        let Some(data) = response.data else {
            return Err(
                "GitHub pull-request comments response is missing a data payload".to_string(),
            );
        };
        let Some(pull_request) = data
            .repository
            .and_then(|repository| repository.pull_request)
        else {
            return Err(
                "GitHub pull-request comments response is missing a pull request".to_string(),
            );
        };

        comments.extend(
            pull_request
                .comments
                .nodes
                .into_iter()
                .map(review_comment_from_node),
        );
    }

    Ok(comments)
}

/// Parses and combines all comments pages for one oversized review thread.
fn parse_thread_comment_pages(stdout: &str) -> Result<Vec<GitHubReviewCommentNode>, String> {
    let responses: Vec<GitHubThreadCommentsEnvelope> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitHub review-thread comments response: {error}"))?;
    let mut comments = Vec::new();
    for response in responses {
        let Some(data) = response.data else {
            return Err(
                "GitHub review-thread comments response is missing a data payload".to_string(),
            );
        };
        let Some(thread) = data.node else {
            return Err("GitHub review-thread comments response is missing a thread".to_string());
        };

        comments.extend(thread.comments.nodes);
    }

    Ok(comments)
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

/// GraphQL pull-request node carrying the review-threads connection.
#[derive(Deserialize)]
struct GitHubReviewThreadsPullRequest {
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
    #[serde(rename = "pageInfo")]
    page_info: GitHubPageInfo,
}

/// GraphQL pagination state used to identify oversized nested connections.
#[derive(Deserialize)]
struct GitHubPageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
}

/// GraphQL response envelope for pull-request conversation comments.
#[derive(Deserialize)]
struct GitHubPullRequestCommentsEnvelope {
    data: Option<GitHubPullRequestCommentsData>,
}

/// GraphQL `data` payload for pull-request conversation comments.
#[derive(Deserialize)]
struct GitHubPullRequestCommentsData {
    repository: Option<GitHubPullRequestCommentsRepository>,
}

/// GraphQL repository node carrying pull-request conversation comments.
#[derive(Deserialize)]
struct GitHubPullRequestCommentsRepository {
    #[serde(rename = "pullRequest")]
    pull_request: Option<GitHubPullRequestCommentsPullRequest>,
}

/// GraphQL pull-request node carrying its conversation-comment connection.
#[derive(Deserialize)]
struct GitHubPullRequestCommentsPullRequest {
    comments: GitHubReviewCommentsConnection,
}

/// GraphQL response envelope for one review thread's comments.
#[derive(Deserialize)]
struct GitHubThreadCommentsEnvelope {
    data: Option<GitHubThreadCommentsData>,
}

/// GraphQL `data` payload for one review thread's comments.
#[derive(Deserialize)]
struct GitHubThreadCommentsData {
    node: Option<GitHubThreadCommentsNode>,
}

/// GraphQL review-thread node carrying its complete comments connection.
#[derive(Deserialize)]
struct GitHubThreadCommentsNode {
    comments: GitHubReviewCommentsConnection,
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use mockall::Sequence;

    use super::*;
    use crate::ReviewRequestMetadataFieldUpdate;
    use crate::command::{ForgeCommandOutput, MockForgeCommandRunner};

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
    async fn authenticated_review_request_metadata_loads_current_pull_request() {
        // Arrange
        let remote = github_remote();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .withf({
                let remote = remote.clone();

                move |command| command == &view_metadata_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_metadata_json())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let metadata = adapter
            .authenticated_review_request_metadata(remote, "#42".to_string())
            .await
            .expect("GitHub metadata lookup should succeed");

        // Assert
        assert_eq!(
            metadata,
            ReviewRequestMetadata {
                body: "Current body.".to_string(),
                title: "Add forge review support".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn sync_authenticated_review_request_metadata_edits_changed_pull_request() {
        // Arrange
        let remote = github_remote();
        let input = UpdateReviewRequestInput {
            body: Some(reconciled_field("Current body.", "Updated body.")),
            title: Some(reconciled_field(
                "Add forge review support",
                "Refine forge review support",
            )),
        };
        let edit = ReviewRequestMetadataEdit {
            body: Some("Updated body.".to_string()),
            title: Some("Refine forge review support".to_string()),
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
                let edit = edit.clone();

                move |command| command == &edit_metadata_command(&remote, "42", &edit)
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
        let summary = adapter
            .sync_authenticated_review_request_metadata(remote, "#42".to_string(), input)
            .await
            .expect("GitHub metadata sync should succeed");

        // Assert
        assert_eq!(summary.display_id, "#42");
    }

    #[tokio::test]
    async fn sync_authenticated_review_request_metadata_skips_edit_when_unchanged() {
        // Arrange
        let remote = github_remote();
        let input = UpdateReviewRequestInput {
            body: Some(reconciled_field("Current body.", "Current body.")),
            title: Some(reconciled_field(
                "Add forge review support",
                "Add forge review support",
            )),
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
        let summary = adapter
            .sync_authenticated_review_request_metadata(remote, "#42".to_string(), input)
            .await
            .expect("GitHub metadata sync should succeed");

        // Assert
        assert_eq!(summary.display_id, "#42");
    }

    #[test]
    fn edit_metadata_command_can_update_only_title() {
        // Arrange
        let remote = github_remote();
        let edit = ReviewRequestMetadataEdit {
            body: None,
            title: Some("Manual-safe title".to_string()),
        };

        // Act
        let command = edit_metadata_command(&remote, "42", &edit);

        // Assert
        assert_eq!(
            command,
            github_command(
                &remote,
                vec![
                    "pr".to_string(),
                    "edit".to_string(),
                    "42".to_string(),
                    "--repo".to_string(),
                    remote.project_path(),
                    "--title".to_string(),
                    "Manual-safe title".to_string(),
                ],
            )
        );
    }

    #[test]
    fn edit_metadata_command_can_update_only_body() {
        // Arrange
        let remote = github_remote();
        let edit = ReviewRequestMetadataEdit {
            body: Some("Manual-safe body".to_string()),
            title: None,
        };

        // Act
        let command = edit_metadata_command(&remote, "42", &edit);

        // Assert
        assert_eq!(
            command,
            github_command(
                &remote,
                vec![
                    "pr".to_string(),
                    "edit".to_string(),
                    "42".to_string(),
                    "--repo".to_string(),
                    remote.project_path(),
                    "--body".to_string(),
                    "Manual-safe body".to_string(),
                ],
            )
        );
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
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &pull_request_comments_command(&remote, "42")
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(github_pull_request_comments_json())) })
            });
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
    async fn fetch_authenticated_review_comment_snapshot_loads_oversized_thread_comments() {
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
            .returning(|_| Box::pin(async { Ok(success_output(github_oversized_thread_json())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &pull_request_comments_command(&remote, "42")
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(github_empty_pull_request_comments_json())) })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &thread_comments_command(&remote, "thread-large")
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(github_thread_comment_pages_json())) })
            });
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let snapshot = adapter
            .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
            .await
            .expect("oversized review thread should load all comments");

        // Assert
        assert_eq!(snapshot.threads.len(), 1);
        assert_eq!(snapshot.threads[0].comments.len(), 2);
        assert_eq!(snapshot.threads[0].comments[0].body, "First page");
        assert_eq!(snapshot.threads[0].comments[1].body, "Second page");
    }

    #[tokio::test]
    async fn fetch_authenticated_review_comment_snapshot_preserves_thread_parse_failure() {
        // Arrange
        let remote = github_remote();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .withf({
                let remote = remote.clone();

                move |command| command == &review_threads_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output("not-json".to_string())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let error = adapter
            .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
            .await
            .expect_err("invalid review threads should fail");

        // Assert
        assert!(matches!(
            error,
            ReviewRequestError::OperationFailed { forge_kind, message }
                if forge_kind == ForgeKind::GitHub
                    && message.contains("invalid GitHub review-threads response")
        ));
    }

    #[tokio::test]
    async fn fetch_authenticated_review_comment_snapshot_preserves_pr_comment_parse_failure() {
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
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &pull_request_comments_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output("not-json".to_string())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let error = adapter
            .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
            .await
            .expect_err("invalid pull-request comments should fail");

        // Assert
        assert!(matches!(
            error,
            ReviewRequestError::OperationFailed { forge_kind, message }
                if forge_kind == ForgeKind::GitHub
                    && message.contains("invalid GitHub pull-request comments response")
        ));
    }

    #[tokio::test]
    async fn fetch_authenticated_review_comment_snapshot_preserves_thread_comment_parse_failure() {
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
            .returning(|_| Box::pin(async { Ok(success_output(github_oversized_thread_json())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &pull_request_comments_command(&remote, "42")
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(github_empty_pull_request_comments_json())) })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &thread_comments_command(&remote, "thread-large")
            })
            .returning(|_| Box::pin(async { Ok(success_output("not-json".to_string())) }));
        let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let error = adapter
            .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
            .await
            .expect_err("invalid review-thread comments should fail");

        // Assert
        assert!(matches!(
            error,
            ReviewRequestError::OperationFailed { forge_kind, message }
                if forge_kind == ForgeKind::GitHub
                    && message.contains("invalid GitHub review-thread comments response")
        ));
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
    fn parse_review_thread_pages_rejects_missing_data() {
        // Arrange
        let stdout = "[{\"data\": null}]";

        // Act
        let error = parse_review_thread_pages(stdout)
            .err()
            .expect("null data payload should be rejected");

        // Assert
        assert!(
            error.contains("missing a data payload"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parse_review_thread_pages_rejects_missing_pull_request() {
        // Arrange
        let stdout = "[{\"data\": {\"repository\": {\"pullRequest\": null}}}]";

        // Act
        let error = parse_review_thread_pages(stdout)
            .err()
            .expect("null pull request should be rejected");

        // Assert
        assert!(
            error.contains("missing a pull request"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn paginated_snapshot_parsers_return_empty_collections_for_empty_connections() {
        // Arrange
        let thread_stdout = r#"[{"data": {"repository": {"pullRequest": {
            "reviewThreads": {"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": null}}
        }}}}]"#;
        let comment_stdout = github_empty_pull_request_comments_json();

        // Act
        let threads = parse_review_thread_pages(thread_stdout)
            .expect("empty review thread list should parse");
        let comments = parse_pull_request_comment_pages(&comment_stdout)
            .expect("empty pull-request comments should parse");

        // Assert
        assert!(threads.is_empty());
        assert_eq!(comments, [] as [crate::model::ReviewComment; 0]);
    }

    #[test]
    fn paginated_comment_parsers_reject_missing_graphql_nodes() {
        // Arrange
        let missing_data = "[{\"data\": null}]";
        let missing_pull_request = "[{\"data\": {\"repository\": {\"pullRequest\": null}}}]";
        let missing_thread = "[{\"data\": {\"node\": null}}]";

        // Act
        let pull_request_data_error = parse_pull_request_comment_pages(missing_data)
            .expect_err("missing pull-request data should fail");
        let pull_request_error = parse_pull_request_comment_pages(missing_pull_request)
            .expect_err("missing pull request should fail");
        let thread_data_error = parse_thread_comment_pages(missing_data)
            .err()
            .expect("missing thread data should fail");
        let thread_error = parse_thread_comment_pages(missing_thread)
            .err()
            .expect("missing thread should fail");

        // Assert
        assert!(pull_request_data_error.contains("missing a data payload"));
        assert!(pull_request_error.contains("missing a pull request"));
        assert!(thread_data_error.contains("missing a data payload"));
        assert!(thread_error.contains("missing a thread"));
    }

    #[test]
    fn paginated_snapshot_parsers_preserve_invalid_json_context() {
        // Arrange
        let invalid_json = "not-json";

        // Act
        let thread_error = parse_review_thread_pages(invalid_json)
            .err()
            .expect("invalid review-thread JSON should fail");
        let pull_request_error = parse_pull_request_comment_pages(invalid_json)
            .expect_err("invalid pull-request comment JSON should fail");
        let thread_comment_error = parse_thread_comment_pages(invalid_json)
            .err()
            .expect("invalid thread-comment JSON should fail");

        // Assert
        assert!(thread_error.contains("invalid GitHub review-threads response"));
        assert!(pull_request_error.contains("invalid GitHub pull-request comments response"));
        assert!(thread_comment_error.contains("invalid GitHub review-thread comments response"));
    }

    #[test]
    fn snapshot_graphql_commands_request_pagination_and_slurped_pages() {
        // Arrange
        let remote = github_remote();
        let commands = [
            review_threads_command(&remote, "42"),
            pull_request_comments_command(&remote, "42"),
            thread_comments_command(&remote, "thread-1"),
        ];

        // Act
        let all_request_pagination = commands.iter().all(|command| {
            command
                .arguments
                .iter()
                .any(|argument| argument == "--paginate")
                && command
                    .arguments
                    .iter()
                    .any(|argument| argument == "--slurp")
                && command
                    .arguments
                    .iter()
                    .any(|argument| argument.contains("$endCursor"))
        });

        // Assert
        assert!(all_request_pagination);
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
        serde_json::json!([
            review_threads_page(vec![review_thread_node(ReviewThreadFixture {
                comments: vec![
                    review_comment_node(Some("alice"), "Why aren't we handling None?"),
                    review_comment_node(Some("bob"), "Good catch. Will fix."),
                ],
                diff_side: "RIGHT",
                has_next_comment_page: false,
                id: "thread-1",
                is_resolved: false,
                line: Some(42),
                path: "src/foo.rs",
                subject_type: "LINE",
            })]),
            review_threads_page(vec![
                review_thread_node(ReviewThreadFixture {
                    comments: vec![review_comment_node(None, "Resolved thread.")],
                    diff_side: "LEFT",
                    has_next_comment_page: false,
                    id: "thread-2",
                    is_resolved: true,
                    line: Some(15),
                    path: "src/bar.rs",
                    subject_type: "LINE",
                }),
                review_thread_node(ReviewThreadFixture {
                    comments: Vec::new(),
                    diff_side: "RIGHT",
                    has_next_comment_page: false,
                    id: "thread-3",
                    is_resolved: false,
                    line: None,
                    path: "Cargo.toml",
                    subject_type: "FILE",
                }),
            ]),
        ])
        .to_string()
    }

    fn github_pull_request_comments_json() -> String {
        serde_json::json!([
            pull_request_comments_page(vec![review_comment_node(
                Some("carol"),
                "Overall looks good.",
            )]),
            pull_request_comments_page(vec![review_comment_node(
                None,
                "Ghost conversation comment.",
            )]),
        ])
        .to_string()
    }

    fn github_empty_pull_request_comments_json() -> String {
        serde_json::json!([pull_request_comments_page(Vec::new())]).to_string()
    }

    fn github_oversized_thread_json() -> String {
        serde_json::json!([review_threads_page(vec![review_thread_node(
            ReviewThreadFixture {
                comments: vec![review_comment_node(Some("alice"), "First page")],
                diff_side: "RIGHT",
                has_next_comment_page: true,
                id: "thread-large",
                is_resolved: false,
                line: Some(7),
                path: "src/large.rs",
                subject_type: "LINE",
            }
        )])])
        .to_string()
    }

    fn github_thread_comment_pages_json() -> String {
        serde_json::json!([
            thread_comments_page(vec![review_comment_node(Some("alice"), "First page")]),
            thread_comments_page(vec![review_comment_node(Some("bob"), "Second page")]),
        ])
        .to_string()
    }

    fn review_threads_page(nodes: Vec<serde_json::Value>) -> serde_json::Value {
        let nodes = serde_json::Value::Array(nodes);

        serde_json::json!({
            "data": {
                "repository": {
                    "pullRequest": {
                        "reviewThreads": {
                            "nodes": nodes,
                            "pageInfo": {"hasNextPage": false, "endCursor": null}
                        }
                    }
                }
            }
        })
    }

    struct ReviewThreadFixture<'a> {
        comments: Vec<serde_json::Value>,
        diff_side: &'a str,
        has_next_comment_page: bool,
        id: &'a str,
        is_resolved: bool,
        line: Option<u32>,
        path: &'a str,
        subject_type: &'a str,
    }

    fn review_thread_node(fixture: ReviewThreadFixture<'_>) -> serde_json::Value {
        let comments = serde_json::Value::Array(fixture.comments);

        serde_json::json!({
            "id": fixture.id,
            "isResolved": fixture.is_resolved,
            "isOutdated": false,
            "path": fixture.path,
            "line": fixture.line,
            "startLine": null,
            "diffSide": fixture.diff_side,
            "subjectType": fixture.subject_type,
            "comments": {
                "nodes": comments,
                "pageInfo": {
                    "hasNextPage": fixture.has_next_comment_page,
                    "endCursor": if fixture.has_next_comment_page {
                        Some("cursor-1")
                    } else {
                        None
                    }
                }
            }
        })
    }

    fn pull_request_comments_page(nodes: Vec<serde_json::Value>) -> serde_json::Value {
        let nodes = serde_json::Value::Array(nodes);

        serde_json::json!({
            "data": {
                "repository": {
                    "pullRequest": {
                        "comments": {
                            "nodes": nodes,
                            "pageInfo": {"hasNextPage": false, "endCursor": null}
                        }
                    }
                }
            }
        })
    }

    fn thread_comments_page(nodes: Vec<serde_json::Value>) -> serde_json::Value {
        let nodes = serde_json::Value::Array(nodes);

        serde_json::json!({
            "data": {
                "node": {
                    "comments": {
                        "nodes": nodes,
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }
            }
        })
    }

    fn review_comment_node(author: Option<&str>, body: &str) -> serde_json::Value {
        serde_json::json!({
            "author": author.map(|login| serde_json::json!({"login": login})),
            "body": body
        })
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
        serde_json::json!({
            "body": "Current body.",
            "title": "Add forge review support"
        })
        .to_string()
    }

    fn reconciled_field(current: &str, desired: &str) -> ReviewRequestMetadataFieldUpdate {
        ReviewRequestMetadataFieldUpdate {
            current: current.to_string(),
            desired: desired.to_string(),
        }
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
