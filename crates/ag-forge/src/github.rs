//! GitHub review-request adapter routed through the `gh` CLI.

use std::sync::Arc;

use serde::Deserialize;

use super::{
    CreateReviewRequestInput, ForgeCommand, ForgeCommandRunner, ForgeFuture, ForgeKind,
    ForgeRemote, ReviewCheckFailure, ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot,
    ReviewCommentThread, ReviewRequestAdapter, ReviewRequestAttention, ReviewRequestError,
    ReviewRequestMetadata, ReviewRequestMetadataEdit, ReviewRequestOperations, ReviewRequestState,
    ReviewRequestSummary, SyncReviewRequestMetadataConfig, UpdateReviewRequestInput,
    map_parse_error, normalize_provider_label, operation_failed, parse_remote_url,
    status_summary_parts, strip_port,
};

/// One compact status query replaces `gh pr view` during each review refresh.
///
/// The rollup check names and the base branch's required status checks let
/// bypass readiness wait for required checks that have not reported yet.
const PULL_REQUEST_STATUS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!) { repository(owner: $owner, name: \
     $repo) { pullRequest(number: $number) { number title state url baseRefName headRefName \
     isDraft mergeable mergeStateStatus reviewDecision mergedAt statusCheckRollup { state \
     contexts(first: 100) { nodes { ... on CheckRun { name } ... on StatusContext { context } } \
     pageInfo { hasNextPage } } } reviewThreads(first: 100) { totalCount nodes { isResolved } } \
     baseRef { refUpdateRule { requiredStatusCheckContexts } rules(first: 100) { totalCount nodes \
     { parameters { ... on RequiredStatusChecksParameters { requiredStatusChecks { context } } } \
     } } } viewerCanMergeAsAdmin } } }";

/// Paginated GraphQL query used when a required check may be reported beyond
/// the first page of rollup contexts.
///
/// Each page carries the current rollup state so readiness never pairs newly
/// fetched check names with an earlier success.
const STATUS_CONTEXTS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) { \
     repository(owner: $owner, name: $repo) { pullRequest(number: $number) { statusCheckRollup { \
     state contexts(first: 100, after: $endCursor) { nodes { ... on CheckRun { name } ... on \
     StatusContext { context } } pageInfo { hasNextPage endCursor } } } } } }";

/// Paginated query for the head commit, mergeability, and check outcomes that
/// may need follow-up work.
///
/// Each page carries the head commit so check outcomes are never combined
/// across a push that landed during pagination.
const PULL_REQUEST_ATTENTION_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) { \
     repository(owner: $owner, name: $repo) { pullRequest(number: $number) { headRefOid mergeable \
     statusCheckRollup { contexts(first: 100, after: $endCursor) { nodes { __typename ... on \
     CheckRun { name status conclusion detailsUrl } ... on StatusContext { context state \
     targetUrl } } pageInfo { hasNextPage endCursor } } } } } }";

/// Paginated GraphQL query used to fetch review threads for one pull request.
const REVIEW_THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) { \
     repository(owner: $owner, name: $repo) { pullRequest(number: $number) { reviewThreads(first: \
     100, after: $endCursor) { nodes { id diffSide isOutdated isResolved line path startLine \
     subjectType comments(first: 100) { nodes { author { login } authorAssociation body \
     viewerDidAuthor } pageInfo { hasNextPage endCursor } } } pageInfo { hasNextPage endCursor } \
     } } } }";
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
     authorAssociation body viewerDidAuthor } pageInfo { hasNextPage endCursor } } } } }";
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

    /// Builds one small GraphQL status request for a pull request.
    fn view_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
        Self::pull_request_graphql_command(remote, PULL_REQUEST_STATUS_QUERY, pull_request_number)
    }

    /// Builds one single-page `gh api graphql` request scoped to a pull
    /// request.
    fn pull_request_graphql_command(
        remote: &ForgeRemote,
        query: &str,
        pull_request_number: &str,
    ) -> ForgeCommand {
        Self::github_command(
            remote,
            vec![
                "api".to_string(),
                "--hostname".to_string(),
                remote.host.clone(),
                "graphql".to_string(),
                "-f".to_string(),
                format!("query={query}"),
                "-F".to_string(),
                format!("owner={}", remote.namespace),
                "-F".to_string(),
                format!("repo={}", remote.project),
                "-F".to_string(),
                format!("number={pull_request_number}"),
            ],
        )
    }

    /// Parses one pull-request status payload from the compact GraphQL
    /// response.
    fn parse_view_pull_request(stdout: &str) -> Result<GitHubViewResponse, String> {
        let response: GitHubViewEnvelope = serde_json::from_str(stdout)
            .map_err(|error| format!("invalid GitHub pull-request view response: {error}"))?;

        response
            .data
            .and_then(|data| data.repository)
            .and_then(|repository| repository.pull_request)
            .ok_or_else(|| {
                "GitHub pull-request view response is missing a pull request".to_string()
            })
    }

    /// Builds one paginated GraphQL command for every rollup check run and
    /// commit status.
    fn status_contexts_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
        Self::paginated_graphql_command(
            remote,
            STATUS_CONTEXTS_QUERY,
            vec![
                format!("owner={}", remote.namespace),
                format!("repo={}", remote.project),
                format!("number={pull_request_number}"),
            ],
        )
    }

    /// Parses and combines all `--slurp` pages of rollup contexts into one
    /// rollup.
    ///
    /// The combined state is `SUCCESS` only when every page reported
    /// `SUCCESS`; otherwise it keeps the first non-passing page state, so a
    /// check that started or failed during pagination withholds readiness.
    fn parse_status_check_rollup_pages(stdout: &str) -> Result<GitHubStatusCheckRollup, String> {
        let responses: Vec<GitHubStatusContextsEnvelope> = serde_json::from_str(stdout)
            .map_err(|error| format!("invalid GitHub status-check contexts response: {error}"))?;
        let mut contexts = Vec::new();
        let mut state: Option<String> = None;
        for response in responses {
            let Some(rollup) = response
                .data
                .and_then(|data| data.repository)
                .and_then(|repository| repository.pull_request)
                .and_then(|pull_request| pull_request.status_check_rollup)
            else {
                return Err(
                    "GitHub status-check contexts response is missing a status check rollup"
                        .to_string(),
                );
            };

            if state.as_deref().is_none_or(|state| state == "SUCCESS") {
                state = Some(rollup.state);
            }
            contexts.extend(rollup.contexts.nodes);
        }
        let state = state
            .ok_or_else(|| "GitHub status-check contexts response contains no pages".to_string())?;

        Ok(GitHubStatusCheckRollup {
            contexts: Some(GitHubStatusContextConnection {
                nodes: contexts,
                page_info: None,
            }),
            state,
        })
    }

    /// Builds one paginated GraphQL command for head-commit, mergeability,
    /// and every check fact of one pull request.
    fn attention_command(remote: &ForgeRemote, pull_request_number: &str) -> ForgeCommand {
        Self::paginated_graphql_command(
            remote,
            PULL_REQUEST_ATTENTION_QUERY,
            vec![
                format!("owner={}", remote.namespace),
                format!("repo={}", remote.project),
                format!("number={pull_request_number}"),
            ],
        )
    }

    /// Parses and combines all `--slurp` pages of head-commit, mergeability,
    /// and check facts from the attention query. Review threads are loaded
    /// separately.
    ///
    /// Every page must report the same head commit; otherwise the checks
    /// belong to different commits and the lookup fails instead of reporting
    /// mixed facts.
    fn parse_attention_pages(stdout: &str) -> Result<ReviewRequestAttention, String> {
        let responses: Vec<GitHubAttentionEnvelope> = serde_json::from_str(stdout)
            .map_err(|error| format!("invalid GitHub pull-request attention response: {error}"))?;
        let mut combined: Option<ReviewRequestAttention> = None;
        for response in responses {
            let pull_request = response
                .data
                .and_then(|data| data.repository)
                .and_then(|repository| repository.pull_request)
                .ok_or_else(|| {
                    "GitHub pull-request attention response is missing a pull request".to_string()
                })?;
            let attention = combined.get_or_insert_with(|| ReviewRequestAttention {
                head_commit: pull_request.head_ref_oid.clone(),
                ..ReviewRequestAttention::default()
            });
            if attention.head_commit != pull_request.head_ref_oid {
                return Err(
                    "GitHub pull-request head commit changed while its checks were loading"
                        .to_string(),
                );
            }

            attention.has_merge_conflict |=
                pull_request.mergeable.as_deref() == Some("CONFLICTING");
            let check_contexts = pull_request
                .status_check_rollup
                .into_iter()
                .flat_map(|rollup| rollup.contexts.nodes)
                .flatten();
            for check_context in check_contexts {
                attention.failed_checks.extend(check_context.failure());
                attention.has_pending_checks |= check_context.is_pending();
            }
        }

        combined
            .ok_or_else(|| "GitHub pull-request attention response contains no pages".to_string())
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

    /// Converts one thread awaiting a response from a trusted author, keeping
    /// only comments written by trusted authors.
    ///
    /// Untrusted comments are dropped before the thread is evaluated, so they
    /// can neither request work nor hide a trusted request. Returns `None`
    /// for resolved threads, threads without a trusted comment, and threads
    /// whose latest trusted comment is an Agentty reply.
    fn trusted_review_thread_from_node(
        node: GitHubReviewThreadNode,
    ) -> Option<ReviewCommentThread> {
        let trusted_comments = node
            .comments
            .nodes
            .iter()
            .map(GitHubReviewCommentNode::is_trusted_author)
            .collect::<Vec<_>>();
        let mut thread = Self::review_comment_thread_from_node(node);
        thread.comments = std::mem::take(&mut thread.comments)
            .into_iter()
            .zip(trusted_comments)
            .filter_map(|(comment, is_trusted)| is_trusted.then_some(comment))
            .collect();

        (!thread.comments.is_empty() && thread.is_actionable()).then_some(thread)
    }

    /// Loads one pull request's review threads with every comment page.
    async fn fetch_review_thread_nodes(
        operations: &ReviewRequestOperations,
        remote: &ForgeRemote,
        pull_request_number: &str,
    ) -> Result<Vec<GitHubReviewThreadNode>, ReviewRequestError> {
        let threads_output = operations
            .run_review_command(
                remote,
                Self::review_threads_command(remote, pull_request_number),
                "fetch pull-request review threads",
            )
            .await?;
        let mut thread_nodes = map_parse_error(
            ForgeKind::GitHub,
            Self::parse_review_thread_pages(&threads_output.stdout),
        )?;
        Self::load_oversized_thread_comments(operations, remote, &mut thread_nodes).await?;

        Ok(thread_nodes)
    }

    /// Replaces the first comment page of each oversized thread with its
    /// complete comment list.
    async fn load_oversized_thread_comments(
        operations: &ReviewRequestOperations,
        remote: &ForgeRemote,
        thread_nodes: &mut [GitHubReviewThreadNode],
    ) -> Result<(), ReviewRequestError> {
        for thread_node in thread_nodes {
            if !thread_node.comments.page_info.has_next_page {
                continue;
            }

            let comments_output = operations
                .run_review_command(
                    remote,
                    Self::thread_comments_command(remote, &thread_node.id),
                    "fetch review-thread comments",
                )
                .await?;
            thread_node.comments.nodes = map_parse_error(
                ForgeKind::GitHub,
                Self::parse_thread_comment_pages(&comments_output.stdout),
            )?;
        }

        Ok(())
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
    ///
    /// Fetches every rollup context page only when a required check is
    /// missing from the first page, so bypass readiness never depends on a
    /// truncated check list.
    fn refresh_authenticated_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        let operations = self.operations.clone();

        Box::pin(async move {
            let pull_request_number = Self::parse_display_id(&display_id)?;
            let output = operations
                .run_review_command(
                    &remote,
                    Self::view_command(&remote, &pull_request_number),
                    "refresh pull request",
                )
                .await?;
            let mut pull_request = map_parse_error(
                ForgeKind::GitHub,
                Self::parse_view_pull_request(&output.stdout),
            )?;
            if pull_request.has_unfetched_required_checks() {
                let contexts_output = operations
                    .run_review_command(
                        &remote,
                        Self::status_contexts_command(&remote, &pull_request_number),
                        "fetch pull-request status checks",
                    )
                    .await?;
                let status_check_rollup = map_parse_error(
                    ForgeKind::GitHub,
                    Self::parse_status_check_rollup_pages(&contexts_output.stdout),
                )?;
                pull_request.status_check_rollup = Some(status_check_rollup);
            }

            Ok(pull_request.into_summary())
        })
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

            Self::load_oversized_thread_comments(&operations, &remote, &mut thread_nodes).await?;

            Ok(ReviewCommentSnapshot {
                pr_level_comments,
                threads: thread_nodes
                    .into_iter()
                    .map(Self::review_comment_thread_from_node)
                    .collect(),
            })
        })
    }

    /// Fetches head-commit, mergeability, and every page of check facts
    /// through one paginated GraphQL query, then the review threads awaiting a
    /// response from trusted authors.
    fn fetch_authenticated_review_request_attention(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestAttention, ReviewRequestError>> {
        let operations = self.operations.clone();

        Box::pin(async move {
            let pull_request_number = Self::parse_display_id(&display_id)?;
            let attention_output = operations
                .run_review_command(
                    &remote,
                    Self::attention_command(&remote, &pull_request_number),
                    "fetch pull-request attention",
                )
                .await?;
            let mut attention = map_parse_error(
                ForgeKind::GitHub,
                Self::parse_attention_pages(&attention_output.stdout),
            )?;
            attention.review_threads =
                Self::fetch_review_thread_nodes(&operations, &remote, &pull_request_number)
                    .await?
                    .into_iter()
                    .filter_map(Self::trusted_review_thread_from_node)
                    .collect();

            Ok(attention)
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
    #[serde(default, rename = "authorAssociation")]
    author_association: Option<String>,
    body: String,
    #[serde(default, rename = "viewerDidAuthor")]
    viewer_did_author: bool,
}

impl GitHubReviewCommentNode {
    /// Returns whether the authenticated user wrote this comment or GitHub
    /// reports its author as a repository owner, organization member, or
    /// collaborator.
    ///
    /// Previous contributors stay untrusted: GitHub reports that association
    /// for anyone with a merged commit, with or without write access.
    fn is_trusted_author(&self) -> bool {
        self.viewer_did_author
            || matches!(
                self.author_association.as_deref(),
                Some("OWNER" | "MEMBER" | "COLLABORATOR")
            )
    }
}

/// GraphQL author node for a review comment. The `ghost` author is the only
/// case where `author` is `null` on GitHub today.
#[derive(Deserialize)]
struct GitHubReviewCommentAuthor {
    login: String,
}

/// Outer GraphQL attention response returned by `gh api graphql`.
#[derive(Deserialize)]
struct GitHubAttentionEnvelope {
    data: Option<GitHubAttentionData>,
}

/// GraphQL `data` payload for the attention query.
#[derive(Deserialize)]
struct GitHubAttentionData {
    repository: Option<GitHubAttentionRepository>,
}

/// GraphQL repository node carrying the attention pull-request field.
#[derive(Deserialize)]
struct GitHubAttentionRepository {
    #[serde(rename = "pullRequest")]
    pull_request: Option<GitHubAttentionPullRequest>,
}

/// GitHub pull-request facts returned by the attention query.
#[derive(Deserialize)]
struct GitHubAttentionPullRequest {
    #[serde(rename = "headRefOid")]
    head_ref_oid: String,
    mergeable: Option<String>,
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<GitHubCheckRollup>,
}

/// Check and commit-status rollup for the pull-request head.
#[derive(Deserialize)]
struct GitHubCheckRollup {
    contexts: GitHubCheckContextConnection,
}

/// One page of check runs and commit statuses in a rollup.
#[derive(Deserialize)]
struct GitHubCheckContextConnection {
    nodes: Vec<Option<GitHubCheckContextNode>>,
}

/// One check run or commit status. Check runs carry `name`, `status`,
/// `conclusion`, and `detailsUrl`; status contexts carry `context`, `state`,
/// and `targetUrl`.
#[derive(Deserialize)]
struct GitHubCheckContextNode {
    conclusion: Option<String>,
    context: Option<String>,
    #[serde(rename = "detailsUrl")]
    details_url: Option<String>,
    name: Option<String>,
    state: Option<String>,
    status: Option<String>,
    #[serde(rename = "targetUrl")]
    target_url: Option<String>,
    #[serde(rename = "__typename")]
    typename: String,
}

impl GitHubCheckContextNode {
    /// Returns the normalized failure when this completed check failed.
    ///
    /// Cancelled, skipped, neutral, stale, and action-required check runs and
    /// expected commit statuses are not failures.
    fn failure(&self) -> Option<ReviewCheckFailure> {
        let (name, outcome, web_url) = match self.typename.as_str() {
            "CheckRun" if self.status.as_deref() == Some("COMPLETED") => (
                self.name.as_deref(),
                self.conclusion.as_deref().filter(|conclusion| {
                    matches!(*conclusion, "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE")
                })?,
                self.details_url.as_deref(),
            ),
            "StatusContext" => (
                self.context.as_deref(),
                self.state
                    .as_deref()
                    .filter(|state| matches!(*state, "FAILURE" | "ERROR"))?,
                self.target_url.as_deref(),
            ),
            _ => return None,
        };

        Some(ReviewCheckFailure {
            name: name.unwrap_or_default().to_string(),
            outcome: normalize_provider_label(outcome),
            web_url: web_url
                .filter(|web_url| !web_url.is_empty())
                .map(str::to_string),
        })
    }

    /// Returns whether this check is still queued, running, or, for a
    /// required commit status, expected but not reported yet.
    fn is_pending(&self) -> bool {
        match self.typename.as_str() {
            "CheckRun" => self.status.as_deref() != Some("COMPLETED"),
            "StatusContext" => matches!(self.state.as_deref(), Some("PENDING" | "EXPECTED")),
            _ => false,
        }
    }
}

/// Outer GraphQL status response returned by `gh api graphql`.
#[derive(Deserialize)]
struct GitHubViewEnvelope {
    data: Option<GitHubViewData>,
}

#[derive(Deserialize)]
struct GitHubViewData {
    repository: Option<GitHubViewRepository>,
}

#[derive(Deserialize)]
struct GitHubViewRepository {
    #[serde(rename = "pullRequest")]
    pull_request: Option<GitHubViewResponse>,
}

/// GitHub pull-request status payload returned by the compact GraphQL query.
#[derive(Deserialize)]
struct GitHubViewResponse {
    #[serde(rename = "baseRef")]
    base_ref: Option<GitHubBaseRefStatus>,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    mergeable: Option<String>,
    #[serde(rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
    #[serde(rename = "mergedAt")]
    merged_at: Option<String>,
    number: u64,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    #[serde(rename = "reviewThreads")]
    review_threads: Option<GitHubReviewThreadStatusConnection>,
    state: String,
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<GitHubStatusCheckRollup>,
    title: String,
    url: String,
    #[serde(default, rename = "viewerCanMergeAsAdmin")]
    viewer_can_merge_as_admin: bool,
}

impl GitHubViewResponse {
    /// Returns whether a passing rollup lacks a required check that a later
    /// contexts page may still report.
    fn has_unfetched_required_checks(&self) -> bool {
        self.unreported_required_check_count()
            .is_some_and(|count| count > 0)
            && self
                .status_check_rollup
                .as_ref()
                .is_some_and(GitHubStatusCheckRollup::has_next_contexts_page)
    }

    /// Converts the status payload into the normalized summary.
    fn into_summary(self) -> ReviewRequestSummary {
        let state = self.review_request_state();
        let status_summary = self.status_summary();

        ReviewRequestSummary {
            display_id: format!("#{}", self.number),
            forge_kind: ForgeKind::GitHub,
            source_branch: self.head_ref_name,
            state,
            status_summary,
            target_branch: self.base_ref_name,
            title: self.title,
            web_url: self.url,
        }
    }

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

        if self.is_ready() {
            parts.push(ReviewRequestSummary::GITHUB_READY_STATUS_COMPONENT.to_string());
        } else if self.is_bypass_ready() {
            parts.push(ReviewRequestSummary::GITHUB_BYPASS_READY_STATUS_COMPONENT.to_string());
        }

        status_summary_parts(&parts)
    }

    /// Rely on GitHub's merge state to apply the repository's merge rules.
    fn is_ready(&self) -> bool {
        self.is_open_non_draft() && self.merge_state_status.as_deref() == Some("CLEAN")
    }

    /// Returns whether the viewer may merge by bypassing branch protections
    /// once checks pass.
    ///
    /// A published pull request qualifies when only a required approval
    /// blocks it. A draft qualifies when it has no merge conflicts and no
    /// unresolved review threads, regardless of approval or branch freshness.
    fn is_bypass_ready(&self) -> bool {
        let is_unblocked = if self.is_draft {
            self.mergeable.as_deref() == Some("MERGEABLE") && self.has_only_resolved_threads()
        } else {
            self.merge_state_status.as_deref() == Some("BLOCKED")
                && self.review_decision.as_deref() == Some("REVIEW_REQUIRED")
        };

        self.state == "OPEN"
            && self.viewer_can_merge_as_admin
            && is_unblocked
            && self.has_passing_required_checks()
    }

    /// Returns whether every reported check passed and every required status
    /// check of the base branch has reported.
    ///
    /// GitHub's rollup aggregates only reported checks, so it stays `SUCCESS`
    /// while a required check is still expected. Missing or partial rule data
    /// counts as an unreported required check.
    fn has_passing_required_checks(&self) -> bool {
        self.unreported_required_check_count() == Some(0)
    }

    /// Counts required checks missing from a passing rollup.
    ///
    /// Returns `None` when the rollup did not pass or the base branch's
    /// required checks are unknown.
    fn unreported_required_check_count(&self) -> Option<usize> {
        let rollup = self
            .status_check_rollup
            .as_ref()
            .filter(|rollup| rollup.state == "SUCCESS")?;
        let required_contexts = self
            .base_ref
            .as_ref()
            .and_then(GitHubBaseRefStatus::required_status_check_contexts)?;
        let reported_contexts = rollup.reported_contexts();

        Some(
            required_contexts
                .iter()
                .filter(|required_context| !reported_contexts.contains(required_context))
                .count(),
        )
    }

    /// Returns whether every review thread is known and resolved.
    ///
    /// A missing connection or one with more threads than the fetched page
    /// counts as unresolved so readiness never relies on partial data.
    fn has_only_resolved_threads(&self) -> bool {
        self.review_threads.as_ref().is_some_and(|threads| {
            threads.nodes.len() == threads.total_count
                && threads.nodes.iter().all(|thread| thread.is_resolved)
        })
    }

    /// Returns whether the pull request is open and published for review.
    fn is_open_non_draft(&self) -> bool {
        self.state == "OPEN" && !self.is_draft
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

/// Aggregate check and commit-status state for the pull-request head.
#[derive(Deserialize)]
struct GitHubStatusCheckRollup {
    contexts: Option<GitHubStatusContextConnection>,
    state: String,
}

impl GitHubStatusCheckRollup {
    /// Returns whether GitHub reported more contexts than the fetched page.
    fn has_next_contexts_page(&self) -> bool {
        self.contexts
            .as_ref()
            .and_then(|contexts| contexts.page_info.as_ref())
            .is_some_and(|page_info| page_info.has_next_page)
    }

    /// Returns the names of check runs and commit statuses reported so far.
    fn reported_contexts(&self) -> Vec<&str> {
        self.contexts
            .iter()
            .flat_map(|contexts| &contexts.nodes)
            .filter_map(GitHubStatusContextNode::label)
            .collect()
    }
}

/// Check runs and commit statuses fetched from the rollup.
#[derive(Deserialize)]
struct GitHubStatusContextConnection {
    nodes: Vec<GitHubStatusContextNode>,
    #[serde(rename = "pageInfo")]
    page_info: Option<GitHubPageInfo>,
}

/// GraphQL response envelope for paginated rollup contexts.
#[derive(Deserialize)]
struct GitHubStatusContextsEnvelope {
    data: Option<GitHubStatusContextsData>,
}

/// GraphQL `data` payload for paginated rollup contexts.
#[derive(Deserialize)]
struct GitHubStatusContextsData {
    repository: Option<GitHubStatusContextsRepository>,
}

/// GraphQL repository node carrying one pull request's rollup contexts.
#[derive(Deserialize)]
struct GitHubStatusContextsRepository {
    #[serde(rename = "pullRequest")]
    pull_request: Option<GitHubStatusContextsPullRequest>,
}

/// GraphQL pull-request node carrying its status-check rollup.
#[derive(Deserialize)]
struct GitHubStatusContextsPullRequest {
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<GitHubStatusContextsRollup>,
}

/// GraphQL rollup node carrying one page of contexts and the rollup state
/// at the time that page was fetched.
#[derive(Deserialize)]
struct GitHubStatusContextsRollup {
    contexts: GitHubStatusContextConnection,
    state: String,
}

/// One check run (`name`) or commit status (`context`) in the rollup.
#[derive(Deserialize)]
struct GitHubStatusContextNode {
    context: Option<String>,
    name: Option<String>,
}

impl GitHubStatusContextNode {
    /// Returns the name branch rules use to require this check.
    fn label(&self) -> Option<&str> {
        self.name.as_deref().or(self.context.as_deref())
    }
}

/// Required status checks configured for the pull-request base branch.
#[derive(Deserialize)]
struct GitHubBaseRefStatus {
    #[serde(rename = "refUpdateRule")]
    ref_update_rule: Option<GitHubRefUpdateRuleStatus>,
    rules: Option<GitHubRepositoryRuleConnection>,
}

impl GitHubBaseRefStatus {
    /// Returns required check names from branch protection and rulesets.
    ///
    /// Returns `None` when ruleset data is missing or spans more than the
    /// fetched page, so callers never rely on a partial requirement list.
    fn required_status_check_contexts(&self) -> Option<Vec<&str>> {
        let rules = self.rules.as_ref()?;
        if rules.nodes.len() != rules.total_count {
            return None;
        }

        let ruleset_contexts = rules
            .nodes
            .iter()
            .filter_map(|rule| rule.parameters.as_ref())
            .flat_map(|parameters| &parameters.required_status_checks)
            .map(|check| check.context.as_str());
        let protection_contexts = self
            .ref_update_rule
            .iter()
            .flat_map(|rule| rule.required_status_check_contexts.iter().flatten())
            .flatten()
            .map(String::as_str);

        Some(ruleset_contexts.chain(protection_contexts).collect())
    }
}

/// Classic branch-protection requirements visible to the viewer.
#[derive(Deserialize)]
struct GitHubRefUpdateRuleStatus {
    #[serde(rename = "requiredStatusCheckContexts")]
    required_status_check_contexts: Option<Vec<Option<String>>>,
}

/// Ruleset rules that apply to the pull-request base branch.
#[derive(Deserialize)]
struct GitHubRepositoryRuleConnection {
    nodes: Vec<GitHubRepositoryRuleNode>,
    #[serde(rename = "totalCount")]
    total_count: usize,
}

/// One ruleset rule; only required-status-check parameters are selected.
#[derive(Deserialize)]
struct GitHubRepositoryRuleNode {
    parameters: Option<GitHubRepositoryRuleParameters>,
}

/// Selected ruleset rule parameters.
#[derive(Deserialize)]
struct GitHubRepositoryRuleParameters {
    #[serde(default, rename = "requiredStatusChecks")]
    required_status_checks: Vec<GitHubRequiredStatusCheck>,
}

/// One status check a ruleset requires before merging.
#[derive(Deserialize)]
struct GitHubRequiredStatusCheck {
    context: String,
}

/// First page of review-thread resolution states for readiness checks.
#[derive(Deserialize)]
struct GitHubReviewThreadStatusConnection {
    nodes: Vec<GitHubReviewThreadStatusNode>,
    #[serde(rename = "totalCount")]
    total_count: usize,
}

/// Resolution state of one review thread.
#[derive(Deserialize)]
struct GitHubReviewThreadStatusNode {
    #[serde(rename = "isResolved")]
    is_resolved: bool,
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
