//! GitHub review-request adapter routed through the `gh` CLI.

use std::sync::Arc;

use serde::Deserialize;

use super::{
    CreateReviewRequestInput, ForgeCommand, ForgeCommandRunner, ForgeFuture, ForgeKind,
    ForgeRemote, ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot,
    ReviewCommentThread, ReviewRequestAdapter, ReviewRequestError, ReviewRequestMetadata,
    ReviewRequestMetadataEdit, ReviewRequestOperations, ReviewRequestState, ReviewRequestSummary,
    SyncReviewRequestMetadataConfig, UpdateReviewRequestInput, map_parse_error,
    normalize_provider_label, operation_failed, parse_remote_url, status_summary_parts, strip_port,
};

/// Paginated GraphQL query used to fetch review threads for one pull request.
const REVIEW_THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) { \
     repository(owner: $owner, name: $repo) { pullRequest(number: $number) { reviewThreads(first: \
     100, after: $endCursor) { nodes { id diffSide isOutdated isResolved line path startLine \
     subjectType comments(first: 100) { nodes { author { login } body viewerDidAuthor } pageInfo \
     { hasNextPage endCursor } } } pageInfo { hasNextPage endCursor } } } } }";
/// Paginated GraphQL query used to fetch pull-request conversation comments.
const PULL_REQUEST_COMMENTS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) { \
     repository(owner: $owner, name: $repo) { pullRequest(number: $number) { comments(first: 100, \
     after: $endCursor) { nodes { author { login } body viewerDidAuthor } pageInfo { hasNextPage \
     endCursor } } } } }";
/// Paginated GraphQL query used when one review thread exceeds 100 comments.
const THREAD_COMMENTS_QUERY: &str =
    "query($threadId: ID!, $endCursor: String) { node(id: $threadId) { ... on \
     PullRequestReviewThread { comments(first: 100, after: $endCursor) { nodes { author { login } \
     body viewerDidAuthor } pageInfo { hasNextPage endCursor } } } } }";
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

    /// Builds the `gh auth status` command for one GitHub host.
    fn auth_status_command(remote: &ForgeRemote) -> ForgeCommand {
        Self::github_command(
            remote,
            vec![
                "auth".to_string(),
                "status".to_string(),
                "--hostname".to_string(),
                remote.host.clone(),
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

    /// Builds the `gh api` lookup command for open pull requests matching
    /// `source_branch`.
    fn lookup_command(remote: &ForgeRemote, source_branch: &str) -> ForgeCommand {
        Self::github_command(
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

    /// Parses one optional display id from a GitHub pull-request lookup
    /// response.
    fn parse_lookup_display_id(stdout: &str) -> Result<Option<String>, String> {
        let pull_requests: Vec<GitHubLookupResponse> = serde_json::from_str(stdout)
            .map_err(|error| format!("invalid GitHub pull-request lookup response: {error}"))?;

        Ok(pull_requests
            .first()
            .map(|pull_request| format!("#{}", pull_request.number)))
    }

    /// Builds the `gh pr create` command for `input`.
    ///
    /// GitHub pull requests default to draft so session-published review
    /// requests do not appear ready for merge before the user chooses to
    /// mark them ready. When a session worktree is available, the command
    /// runs there so `gh` does not inherit a stale process cwd and fail
    /// when it shells out to `git`.
    fn create_command(remote: &ForgeRemote, input: &CreateReviewRequestInput) -> ForgeCommand {
        Self::github_command(
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
        Self::github_command(
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

    /// Builds GitHub-specific metadata view and edit configuration.
    fn metadata_sync_config() -> SyncReviewRequestMetadataConfig {
        SyncReviewRequestMetadataConfig {
            edit_metadata_command: Self::edit_metadata_command,
            edit_operation: "update pull-request metadata",
            parse_display_id: Self::parse_display_id,
            parse_metadata_response: Self::parse_metadata_response,
            view_metadata_command: Self::view_metadata_command,
            view_operation: "view pull-request metadata",
        }
    }

    /// Builds the `gh pr edit` command for updating one pull-request
    /// title/body.
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

        Self::github_command(remote, arguments)
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

    /// Builds the `gh pr view` command that reads title/body metadata used for
    /// change detection before editing a pull request.
    fn view_metadata_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
        Self::github_command(
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

    /// Builds one paginated `gh api graphql` command that fetches review
    /// threads.
    fn review_threads_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
        Self::paginated_graphql_command(
            remote,
            REVIEW_THREADS_QUERY,
            vec![
                format!("owner={}", remote.namespace),
                format!("repo={}", remote.project),
                format!("number={pull_request_number}"),
            ],
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

        Self::github_command(remote, arguments)
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

    /// Builds one paginated GraphQL command for pull-request conversation
    /// comments.
    fn pull_request_comments_command(
        remote: &ForgeRemote,
        pull_request_number: &str,
    ) -> ForgeCommand {
        Self::paginated_graphql_command(
            remote,
            PULL_REQUEST_COMMENTS_QUERY,
            vec![
                format!("owner={}", remote.namespace),
                format!("repo={}", remote.project),
                format!("number={pull_request_number}"),
            ],
        )
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
                    .map(Self::review_comment_from_node),
            );
        }

        Ok(comments)
    }

    /// Converts one GraphQL comment node into the forge-neutral representation.
    fn review_comment_from_node(node: GitHubReviewCommentNode) -> ReviewComment {
        ReviewComment {
            author: node
                .author
                .map_or_else(|| "ghost".to_string(), |author| author.login),
            authored_by_current_user: node.viewer_did_author,
            body: node.body,
        }
    }

    /// Builds one paginated GraphQL command for all comments in one review
    /// thread.
    fn thread_comments_command(remote: &ForgeRemote, thread_id: &str) -> ForgeCommand {
        Self::paginated_graphql_command(
            remote,
            THREAD_COMMENTS_QUERY,
            vec![format!("threadId={thread_id}")],
        )
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
                return Err(
                    "GitHub review-thread comments response is missing a thread".to_string()
                );
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
            anchor_side: Self::github_anchor_side(&node),
            comments: node
                .comments
                .nodes
                .into_iter()
                .map(Self::review_comment_from_node)
                .collect(),
            id: node.id,
            is_outdated: Some(node.is_outdated),
            is_resolved: node.is_resolved,
            line,
            path: node.path,
            start_line: node.start_line,
        }
    }

    /// Converts GitHub's diff-side labels into Agentty's normalized anchor
    /// side.
    fn github_anchor_side(node: &GitHubReviewThreadNode) -> ReviewCommentAnchorSide {
        if node.subject_type == "FILE" || node.line.is_none() {
            return ReviewCommentAnchorSide::File;
        }

        match node.diff_side.as_str() {
            "LEFT" => ReviewCommentAnchorSide::Old,
            _ => ReviewCommentAnchorSide::New,
        }
    }

    /// Builds one `gh api graphql` mutation that replies to a review thread.
    fn reply_to_thread_command(remote: &ForgeRemote, thread_id: &str, body: &str) -> ForgeCommand {
        Self::github_command(
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
        Self::github_command(
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
}

impl ReviewRequestAdapter for GitHubReviewRequestAdapter {
    fn ensure_authenticated(
        &self,
        remote: &ForgeRemote,
    ) -> ForgeFuture<Result<(), ReviewRequestError>> {
        self.operations
            .ensure_authenticated_future(remote.clone(), Self::auth_status_command)
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
            Self::lookup_command,
            "find pull request",
            Self::parse_lookup_display_id,
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
            let create_command = Self::create_command(&remote, &input);
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
            Self::parse_display_id,
            Self::view_command,
            "refresh pull request",
            Self::parse_view_response,
        )
    }

    /// Loads current pull-request title/body metadata.
    fn authenticated_review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestMetadata, ReviewRequestError>> {
        self.operations.review_request_metadata_future(
            remote,
            display_id,
            &Self::metadata_sync_config(),
        )
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
            &Self::metadata_sync_config(),
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
            let pull_request_number = Self::parse_display_id(&display_id)?;
            let threads_output = operations
                .run_review_command(
                    &remote,
                    Self::review_threads_command(&remote, &pull_request_number),
                    "fetch pull-request review threads",
                )
                .await?;
            let mut thread_nodes = map_parse_error(
                ForgeKind::GitHub,
                Self::parse_review_thread_pages(&threads_output.stdout),
            )?;
            let comments_output = operations
                .run_review_command(
                    &remote,
                    Self::pull_request_comments_command(&remote, &pull_request_number),
                    "fetch pull-request conversation comments",
                )
                .await?;
            let pr_level_comments = map_parse_error(
                ForgeKind::GitHub,
                Self::parse_pull_request_comment_pages(&comments_output.stdout),
            )?;

            for thread_node in &mut thread_nodes {
                if !thread_node.comments.page_info.has_next_page {
                    continue;
                }

                let comments_output = operations
                    .run_review_command(
                        &remote,
                        Self::thread_comments_command(&remote, &thread_node.id),
                        "fetch review-thread comments",
                    )
                    .await?;
                thread_node.comments.nodes = map_parse_error(
                    ForgeKind::GitHub,
                    Self::parse_thread_comment_pages(&comments_output.stdout),
                )?;
            }

            Ok(ReviewCommentSnapshot {
                pr_level_comments,
                threads: thread_nodes
                    .into_iter()
                    .map(Self::review_comment_thread_from_node)
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
            Self::parse_display_id(&display_id)?;
            operations
                .run_review_command(
                    &remote,
                    Self::reply_to_thread_command(&remote, &thread_id, &body),
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
            Self::parse_display_id(&display_id)?;
            operations
                .run_review_command(
                    &remote,
                    Self::resolve_thread_command(&remote, &thread_id),
                    "resolve pull-request review thread",
                )
                .await?;

            Ok(())
        })
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
    id: String,
    #[serde(rename = "isOutdated")]
    is_outdated: bool,
    #[serde(rename = "isResolved")]
    is_resolved: bool,
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
    #[serde(default, rename = "viewerDidAuthor")]
    viewer_did_author: bool,
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
#[path = "github_test.rs"]
mod tests;
