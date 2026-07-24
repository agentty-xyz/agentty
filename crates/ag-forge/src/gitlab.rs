//! GitLab review-request adapter routed through the `glab` CLI.

use std::sync::Arc;

use serde::Deserialize;
use url::{Url, form_urlencoded};

use super::{
    CreateReviewRequestInput, ForgeCommand, ForgeCommandRunner, ForgeFuture, ForgeKind,
    ForgeRemote, RequestedReview, RequestedReviewAudience, ReviewComment, ReviewCommentAnchorSide,
    ReviewCommentSnapshot, ReviewCommentThread, ReviewRequestAdapter, ReviewRequestError,
    ReviewRequestMetadata, ReviewRequestMetadataEdit, ReviewRequestOperations, ReviewRequestState,
    ReviewRequestSummary, SyncReviewRequestMetadataConfig, UpdateReviewRequestInput,
    is_gitlab_host, map_parse_error, normalize_provider_label, parse_remote_url,
    status_summary_parts, strip_port,
};

/// Maximum requested-review rows loaded from `glab` for one refresh.
const REQUESTED_REVIEW_LIMIT: usize = 100;

/// GitLab merge-request adapter that normalizes `glab` command output.
#[derive(Clone)]
pub(crate) struct GitLabReviewRequestAdapter {
    operations: ReviewRequestOperations,
}

impl GitLabReviewRequestAdapter {
    /// Builds one GitLab adapter from a forge command runner.
    pub(crate) fn new(command_runner: Arc<dyn ForgeCommandRunner>) -> Self {
        Self {
            operations: ReviewRequestOperations::new(command_runner),
        }
    }

    /// Returns normalized GitLab remote metadata when `repo_url` is supported.
    pub(crate) fn detect_remote(repo_url: &str) -> Option<ForgeRemote> {
        let parsed_remote = parse_remote_url(repo_url)?;
        if !is_gitlab_host(strip_port(&parsed_remote.host)) {
            return None;
        }

        Some(parsed_remote.into_forge_remote(ForgeKind::GitLab))
    }
}

impl ReviewRequestAdapter for GitLabReviewRequestAdapter {
    fn ensure_authenticated(
        &self,
        remote: &ForgeRemote,
    ) -> ForgeFuture<Result<(), ReviewRequestError>> {
        self.operations
            .ensure_authenticated_future(remote.clone(), auth_status_command)
    }

    /// Finds one existing merge request for `source_branch`.
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
            "find merge request",
            parse_lookup_display_id,
            move |remote, display_id| {
                adapter.refresh_authenticated_review_request(remote, display_id)
            },
        )
    }

    /// Creates one new draft merge request from `input`.
    fn create_authenticated_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        let adapter = self.clone();
        let operations = self.operations.clone();

        Box::pin(async move {
            let create_command = create_command(&remote, &input);
            let output = operations
                .run_review_command(&remote, create_command, "create merge request")
                .await?;
            let display_id =
                map_parse_error(remote.forge_kind, parse_create_display_id(&output.stdout))?;

            adapter
                .refresh_authenticated_review_request(remote, display_id)
                .await
        })
    }

    /// Refreshes one existing merge request by display id.
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
            "refresh merge request",
            parse_view_response,
        )
    }

    /// Loads current merge-request title/description metadata.
    fn authenticated_review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestMetadata, ReviewRequestError>> {
        self.operations
            .review_request_metadata_future(remote, display_id, &metadata_sync_config())
    }

    /// Checks current merge-request metadata and updates fields that still
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

    /// Fetches merge-request discussions through GitLab's REST API and
    /// normalizes diff notes plus review-request-wide notes for
    /// requested-review detail.
    fn fetch_authenticated_review_comment_snapshot(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewCommentSnapshot, ReviewRequestError>> {
        self.operations.fetch_review_comment_snapshot_future(
            remote,
            display_id,
            parse_display_id,
            discussions_command,
            "fetch merge-request discussions",
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
            let merge_request_iid = parse_display_id(&display_id)?;
            operations
                .run_review_command(
                    &remote,
                    reply_to_thread_command(&remote, &merge_request_iid, &thread_id, &body),
                    "reply to merge-request discussion",
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
            let merge_request_iid = parse_display_id(&display_id)?;
            operations
                .run_review_command(
                    &remote,
                    resolve_thread_command(&remote, &merge_request_iid, &thread_id),
                    "resolve merge-request discussion",
                )
                .await?;

            Ok(())
        })
    }

    /// Lists open merge requests in `remote` that request review from the
    /// current authenticated GitLab user.
    fn list_authenticated_requested_reviews(
        &self,
        remote: ForgeRemote,
    ) -> ForgeFuture<Result<Vec<RequestedReview>, ReviewRequestError>> {
        self.operations.list_requested_reviews_future(
            remote,
            requested_reviews_command,
            "list requested merge-request reviews",
            parse_requested_reviews_response,
        )
    }
}

/// Builds GitLab-specific metadata view and edit configuration.
fn metadata_sync_config() -> SyncReviewRequestMetadataConfig {
    SyncReviewRequestMetadataConfig {
        edit_metadata_command: update_metadata_command,
        edit_operation: "update merge-request metadata",
        parse_display_id,
        parse_metadata_response,
        view_metadata_command: view_command,
        view_operation: "view merge-request metadata",
    }
}

/// Builds the `glab auth status` command for one GitLab host.
fn auth_status_command(remote: &ForgeRemote) -> ForgeCommand {
    gitlab_command(
        remote,
        "glab",
        vec![
            "auth".to_string(),
            "status".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
        ],
    )
}

/// Builds the `glab mr list` command for open merge requests matching
/// `source_branch`.
fn lookup_command(remote: &ForgeRemote, source_branch: &str) -> ForgeCommand {
    gitlab_command(
        remote,
        "glab",
        vec![
            "mr".to_string(),
            "list".to_string(),
            "--repo".to_string(),
            remote.web_url.clone(),
            "--source-branch".to_string(),
            source_branch.to_string(),
            "--order".to_string(),
            "created_at".to_string(),
            "--sort".to_string(),
            "desc".to_string(),
            "--per-page".to_string(),
            "1".to_string(),
            "--output".to_string(),
            "json".to_string(),
        ],
    )
}

/// Parses one optional display id from a GitLab merge-request lookup response.
fn parse_lookup_display_id(stdout: &str) -> Result<Option<String>, String> {
    let merge_requests: Vec<GitLabLookupResponse> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitLab merge-request lookup response: {error}"))?;

    Ok(merge_requests
        .first()
        .map(|merge_request| format!("!{}", merge_request.iid)))
}

/// Builds the `glab mr create` command for `input`.
///
/// GitLab merge requests default to draft so session-published review requests
/// do not appear ready for merge before the user chooses to mark them ready.
fn create_command(remote: &ForgeRemote, input: &CreateReviewRequestInput) -> ForgeCommand {
    gitlab_command(
        remote,
        "glab",
        vec![
            "mr".to_string(),
            "create".to_string(),
            "--repo".to_string(),
            remote.web_url.clone(),
            "--draft".to_string(),
            "--source-branch".to_string(),
            input.source_branch.clone(),
            "--target-branch".to_string(),
            input.target_branch.clone(),
            "--title".to_string(),
            input.title.clone(),
            "--description".to_string(),
            input.body.clone().unwrap_or_default(),
            "--yes".to_string(),
        ],
    )
}

/// Parses one merge-request display id from `glab mr create` stdout.
fn parse_create_display_id(stdout: &str) -> Result<String, String> {
    let created_url = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "missing GitLab merge-request URL in create response".to_string())?;
    let created_url = Url::parse(created_url)
        .map_err(|error| format!("invalid GitLab merge-request create response URL: {error}"))?;
    let path_segments = created_url
        .path_segments()
        .ok_or_else(|| "invalid GitLab merge-request create response URL path".to_string())?
        .collect::<Vec<_>>();
    let merge_request_index = path_segments
        .iter()
        .rposition(|segment| *segment == "merge_requests")
        .ok_or_else(|| "missing merge request path segment in create response URL".to_string())?;
    let merge_request_iid = path_segments
        .get(merge_request_index + 1)
        .ok_or_else(|| "missing merge request iid in create response URL".to_string())?;
    let display_id = format!("!{merge_request_iid}");
    parse_display_id_value(&display_id)?;

    Ok(display_id)
}

/// Parses one GitLab merge-request display id into the numeric argument for
/// `glab`.
fn parse_display_id(display_id: &str) -> Result<String, ReviewRequestError> {
    parse_display_id_value(display_id).map_err(|message| ReviewRequestError::OperationFailed {
        forge_kind: ForgeKind::GitLab,
        message,
    })
}

/// Validates one GitLab merge-request display id and returns its numeric value.
fn parse_display_id_value(display_id: &str) -> Result<String, String> {
    let trimmed = display_id.trim().trim_start_matches('!');
    if trimmed.is_empty() || !trimmed.chars().all(|character| character.is_ascii_digit()) {
        return Err(format!(
            "invalid GitLab merge-request display id: `{display_id}`"
        ));
    }

    Ok(trimmed.to_string())
}

/// Builds the `glab mr view` command for one merge-request IID.
fn view_command(remote: &ForgeRemote, merge_request_iid: &str) -> ForgeCommand {
    gitlab_command(
        remote,
        "glab",
        vec![
            "mr".to_string(),
            "view".to_string(),
            merge_request_iid.to_string(),
            "--repo".to_string(),
            remote.web_url.clone(),
            "--output".to_string(),
            "json".to_string(),
        ],
    )
}

/// Parses one merge-request summary from a `glab mr view --output json`
/// response.
fn parse_view_response(stdout: &str) -> Result<ReviewRequestSummary, String> {
    let merge_request: GitLabViewResponse = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitLab merge-request view response: {error}"))?;
    let state = merge_request.review_request_state();
    let status_summary = merge_request.status_summary();

    Ok(ReviewRequestSummary {
        display_id: format!("!{}", merge_request.iid),
        forge_kind: ForgeKind::GitLab,
        source_branch: merge_request.source_branch,
        state,
        status_summary,
        target_branch: merge_request.target_branch,
        title: merge_request.title,
        web_url: merge_request.web_url,
    })
}

/// Parses current merge-request title/description metadata from `glab mr view`
/// JSON.
fn parse_metadata_response(stdout: &str) -> Result<ReviewRequestMetadata, String> {
    let metadata: GitLabMetadataResponse = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitLab merge-request metadata response: {error}"))?;

    Ok(ReviewRequestMetadata {
        body: metadata.description,
        title: metadata.title,
    })
}

/// Builds the `glab mr update` command for updating one merge-request
/// title/description.
fn update_metadata_command(
    remote: &ForgeRemote,
    merge_request_iid: &str,
    edit: &ReviewRequestMetadataEdit,
) -> ForgeCommand {
    let mut arguments = vec![
        "mr".to_string(),
        "update".to_string(),
        merge_request_iid.to_string(),
        "--repo".to_string(),
        remote.web_url.clone(),
    ];
    if let Some(title) = edit.title.as_ref() {
        arguments.extend(["--title".to_string(), title.clone()]);
    }
    if let Some(body) = edit.body.as_ref() {
        arguments.extend(["--description".to_string(), body.clone()]);
    }
    arguments.push("--yes".to_string());

    gitlab_command(remote, "glab", arguments)
}

/// Builds the `glab api` command for merge-request discussions.
fn discussions_command(remote: &ForgeRemote, merge_request_iid: &str) -> ForgeCommand {
    let encoded_project_path: String =
        form_urlencoded::byte_serialize(remote.project_path().as_bytes()).collect();
    let endpoint = format!(
        "/projects/{encoded_project_path}/merge_requests/{merge_request_iid}/discussions?\
         per_page=100"
    );

    gitlab_command(
        remote,
        "glab",
        vec![
            "api".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
            "--paginate".to_string(),
            endpoint,
        ],
    )
}

/// Builds one `glab api` request that replies to a merge-request discussion.
fn reply_to_thread_command(
    remote: &ForgeRemote,
    merge_request_iid: &str,
    thread_id: &str,
    body: &str,
) -> ForgeCommand {
    let endpoint = discussion_endpoint(remote, merge_request_iid, thread_id, Some("notes"));

    gitlab_command(
        remote,
        "glab",
        vec![
            "api".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
            "--method".to_string(),
            "POST".to_string(),
            "--raw-field".to_string(),
            format!("body={body}"),
            endpoint,
        ],
    )
}

/// Builds one `glab api` request that resolves a merge-request discussion.
fn resolve_thread_command(
    remote: &ForgeRemote,
    merge_request_iid: &str,
    thread_id: &str,
) -> ForgeCommand {
    let endpoint = discussion_endpoint(remote, merge_request_iid, thread_id, None);

    gitlab_command(
        remote,
        "glab",
        vec![
            "api".to_string(),
            "--hostname".to_string(),
            remote.host.clone(),
            "--method".to_string(),
            "PUT".to_string(),
            "--field".to_string(),
            "resolved=true".to_string(),
            endpoint,
        ],
    )
}

/// Returns the encoded GitLab discussion endpoint for one optional child
/// resource.
fn discussion_endpoint(
    remote: &ForgeRemote,
    merge_request_iid: &str,
    thread_id: &str,
    child_resource: Option<&str>,
) -> String {
    let encoded_project_path: String =
        form_urlencoded::byte_serialize(remote.project_path().as_bytes()).collect();
    let encoded_thread_id: String = form_urlencoded::byte_serialize(thread_id.as_bytes()).collect();
    let mut endpoint = format!(
        "/projects/{encoded_project_path}/merge_requests/{merge_request_iid}/discussions/\
         {encoded_thread_id}"
    );
    if let Some(child_resource) = child_resource {
        endpoint.push('/');
        endpoint.push_str(child_resource);
    }

    endpoint
}

/// Parses merge-request discussions into inline threads and MR-level comments.
fn parse_review_comment_snapshot_response(stdout: &str) -> Result<ReviewCommentSnapshot, String> {
    let discussions: Vec<GitLabDiscussion> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitLab merge-request discussions response: {error}"))?;
    let mut pr_level_comments = Vec::new();
    let mut threads = Vec::new();

    for discussion in discussions {
        let mut notes = discussion
            .notes
            .into_iter()
            .filter(|note| !note.system)
            .collect::<Vec<_>>();
        if notes.is_empty() {
            continue;
        }

        if let Some(thread) = review_comment_thread_from_discussion(&discussion.id, &notes) {
            threads.push(thread);
        } else {
            pr_level_comments.extend(notes.drain(..).map(review_comment_from_note));
        }
    }

    Ok(ReviewCommentSnapshot {
        pr_level_comments,
        threads,
    })
}

/// Converts one GitLab discussion into an inline thread when it carries a diff
/// note position.
fn review_comment_thread_from_discussion(
    discussion_id: &str,
    notes: &[GitLabDiscussionNote],
) -> Option<ReviewCommentThread> {
    let anchor_note = notes.iter().find(|note| {
        note.note_type.as_deref() == Some("DiffNote") && note.position.as_ref().is_some()
    })?;
    let position = anchor_note.position.as_ref()?;
    let (anchor_side, path, line) = gitlab_anchor_from_position(position);

    Some(ReviewCommentThread {
        anchor_side,
        comments: notes
            .iter()
            .cloned()
            .map(review_comment_from_note)
            .collect(),
        id: discussion_id.to_string(),
        is_outdated: None,
        is_resolved: anchor_note.resolved,
        line,
        path,
        start_line: None,
    })
}

/// Converts one GitLab diff-note position into Agentty's normalized anchor.
fn gitlab_anchor_from_position(
    position: &GitLabDiscussionPosition,
) -> (ReviewCommentAnchorSide, String, Option<u32>) {
    if let Some(new_line) = position.new_line {
        return (
            ReviewCommentAnchorSide::New,
            position
                .new_path
                .clone()
                .or_else(|| position.old_path.clone())
                .unwrap_or_default(),
            Some(new_line),
        );
    }

    if let Some(old_line) = position.old_line {
        return (
            ReviewCommentAnchorSide::Old,
            position
                .old_path
                .clone()
                .or_else(|| position.new_path.clone())
                .unwrap_or_default(),
            Some(old_line),
        );
    }

    (
        ReviewCommentAnchorSide::File,
        position
            .new_path
            .clone()
            .or_else(|| position.old_path.clone())
            .unwrap_or_default(),
        None,
    )
}

/// Converts one GitLab discussion note into the forge-neutral comment shape.
fn review_comment_from_note(note: GitLabDiscussionNote) -> ReviewComment {
    ReviewComment {
        author: note
            .author
            .username
            .or(note.author.name)
            .unwrap_or_default(),
        body: note.body,
    }
}

/// Builds the `glab mr list` command for MRs requesting the current user's
/// review in the selected repository.
fn requested_reviews_command(remote: &ForgeRemote) -> ForgeCommand {
    gitlab_command(
        remote,
        "glab",
        vec![
            "mr".to_string(),
            "list".to_string(),
            "--repo".to_string(),
            remote.web_url.clone(),
            "--reviewer".to_string(),
            "@me".to_string(),
            "--per-page".to_string(),
            REQUESTED_REVIEW_LIMIT.to_string(),
            "--output".to_string(),
            "json".to_string(),
        ],
    )
}

/// Parses GitLab list rows into normalized requested-review rows.
///
/// The current `glab mr list --reviewer @me` surface filters by user reviewer
/// and does not expose a separate reviewer-group audience for listed rows, so
/// GitLab requested reviews are normalized as personal requests.
fn parse_requested_reviews_response(
    stdout: &str,
    remote: &ForgeRemote,
) -> Result<Vec<RequestedReview>, String> {
    let merge_requests: Vec<GitLabRequestedReviewResponse> = serde_json::from_str(stdout)
        .map_err(|error| format!("invalid GitLab requested-review response: {error}"))?;

    Ok(merge_requests
        .into_iter()
        .map(|merge_request| {
            let status_summary = if merge_request.draft {
                Some("Draft".to_string())
            } else {
                None
            };

            RequestedReview {
                audience: RequestedReviewAudience::Personal,
                author: merge_request.author.map_or_else(
                    || "unknown".to_string(),
                    |author| {
                        author
                            .username
                            .or(author.name)
                            .unwrap_or_else(|| "unknown".to_string())
                    },
                ),
                body: merge_request.description,
                comment_snapshot: None,
                display_id: format!("!{}", merge_request.iid),
                forge_kind: ForgeKind::GitLab,
                repository: remote.project_path(),
                status_summary,
                title: merge_request.title,
                updated_at: merge_request.updated_at,
                web_url: merge_request.web_url,
            }
        })
        .collect())
}

/// Builds one base `glab` command with deterministic color settings and the
/// optional session worktree for repository-aware host detection.
fn gitlab_command(
    remote: &ForgeRemote,
    executable: &'static str,
    arguments: Vec<String>,
) -> ForgeCommand {
    ForgeCommand::new(executable, arguments)
        .with_environment("CLICOLOR", "0")
        .with_environment("NO_COLOR", "1")
        .with_environment("GITLAB_HOST", remote.host.clone())
        .with_optional_working_directory(remote.command_working_directory.clone())
}

/// Minimal GitLab list payload used to find an existing merge request.
#[derive(Deserialize)]
struct GitLabLookupResponse {
    iid: u64,
}

/// GitLab list row returned by `glab mr list --output json`.
#[derive(Deserialize)]
struct GitLabRequestedReviewResponse {
    author: Option<GitLabRequestedReviewAuthor>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    draft: bool,
    iid: u64,
    title: String,
    #[serde(rename = "updated_at")]
    updated_at: Option<String>,
    #[serde(rename = "web_url")]
    web_url: String,
}

/// GitLab list author data for the user who opened a requested review.
#[derive(Deserialize)]
struct GitLabRequestedReviewAuthor {
    name: Option<String>,
    username: Option<String>,
}

/// GitLab merge-request JSON payload returned by `glab mr view --output json`.
#[derive(Deserialize)]
struct GitLabViewResponse {
    #[serde(default)]
    draft: bool,
    #[serde(rename = "detailed_merge_status")]
    detailed_merge_status: Option<String>,
    iid: u64,
    #[serde(rename = "merge_status")]
    merge_status: Option<String>,
    #[serde(rename = "merged_at")]
    merged_at: Option<String>,
    #[serde(rename = "source_branch")]
    source_branch: String,
    state: String,
    #[serde(rename = "target_branch")]
    target_branch: String,
    title: String,
    #[serde(rename = "web_url")]
    web_url: String,
}

impl GitLabViewResponse {
    /// Maps GitLab state fields into the normalized review-request state.
    fn review_request_state(&self) -> ReviewRequestState {
        if self.merged_at.is_some() || self.state.eq_ignore_ascii_case("merged") {
            return ReviewRequestState::Merged;
        }

        if matches!(self.state.as_str(), "closed" | "locked") {
            return ReviewRequestState::Closed;
        }

        ReviewRequestState::Open
    }

    /// Formats the provider-specific status summary for the UI.
    fn status_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.draft {
            parts.push("Draft".to_string());
        }

        if let Some(merge_summary) = Self::merge_status_summary(
            self.merge_status.as_deref(),
            self.detailed_merge_status.as_deref(),
        ) {
            parts.push(merge_summary);
        }

        status_summary_parts(&parts)
    }

    /// Formats one GitLab merge-status label for the UI.
    fn merge_status_summary(
        merge_status: Option<&str>,
        detailed_merge_status: Option<&str>,
    ) -> Option<String> {
        let status = detailed_merge_status.or(merge_status)?;

        match status {
            "can_be_merged" | "mergeable" => Some("Mergeable".to_string()),
            "cannot_be_merged" => Some("Conflicts".to_string()),
            "cannot_be_merged_recheck" | "checking" | "unchecked" => Some("Checking".to_string()),
            "ci_still_running" | "commits_status" => Some("Checks pending".to_string()),
            "ci_must_pass" => Some("Checks required".to_string()),
            "discussions_not_resolved" => Some("Discussions unresolved".to_string()),
            "draft_status" | "not_open" => None,
            other => Some(normalize_provider_label(other)),
        }
    }
}

/// GitLab merge-request title/description payload returned by
/// `glab mr view --output json`.
#[derive(Deserialize)]
struct GitLabMetadataResponse {
    #[serde(default)]
    description: String,
    title: String,
}

/// GitLab merge-request discussion returned by the discussions API.
#[derive(Clone, Deserialize)]
struct GitLabDiscussion {
    id: String,
    notes: Vec<GitLabDiscussionNote>,
}

/// One GitLab discussion note, optionally carrying a diff position.
#[derive(Clone, Deserialize)]
struct GitLabDiscussionNote {
    author: GitLabDiscussionAuthor,
    body: String,
    position: Option<GitLabDiscussionPosition>,
    #[serde(default)]
    resolved: bool,
    #[serde(default)]
    system: bool,
    #[serde(rename = "type")]
    note_type: Option<String>,
}

/// Minimal GitLab note author data shown in requested-review detail.
#[derive(Clone, Deserialize)]
struct GitLabDiscussionAuthor {
    name: Option<String>,
    username: Option<String>,
}

/// Minimal GitLab diff position used to anchor inline comments.
#[derive(Clone, Deserialize)]
struct GitLabDiscussionPosition {
    #[serde(rename = "new_line")]
    new_line: Option<u32>,
    #[serde(rename = "new_path")]
    new_path: Option<String>,
    #[serde(rename = "old_line")]
    old_line: Option<u32>,
    #[serde(rename = "old_path")]
    old_path: Option<String>,
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
        let remote = gitlab_remote();
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
            .returning(|_| Box::pin(async { Ok(success_output(r#"[{"iid":42}]"#.to_string())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .find_authenticated_by_source_branch(remote, "feature/forge".to_string())
            .await
            .expect("GitLab lookup should succeed");

        // Assert
        assert_eq!(
            review_request,
            Some(ReviewRequestSummary {
                display_id: "!42".to_string(),
                forge_kind: ForgeKind::GitLab,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Draft, Mergeable".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn find_authenticated_by_source_branch_returns_none_for_empty_lookup_response() {
        // Arrange
        let remote = gitlab_remote();
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
            .returning(|_| Box::pin(async { Ok(success_output("[]".to_string())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .find_authenticated_by_source_branch(remote, "feature/forge".to_string())
            .await
            .expect("GitLab lookup should succeed");

        // Assert
        assert_eq!(review_request, None);
    }

    #[test]
    fn lookup_command_uses_default_open_merge_request_filter() {
        // Arrange
        let remote = gitlab_remote();

        // Act
        let command = lookup_command(&remote, "feature/forge");

        // Assert
        assert!(!command.arguments.contains(&"--all".to_string()));
        assert!(!command.arguments.contains(&"--closed".to_string()));
        assert!(!command.arguments.contains(&"--merged".to_string()));
        assert!(command.arguments.contains(&"--source-branch".to_string()));
    }

    #[tokio::test]
    async fn create_authenticated_review_request_builds_create_command_and_returns_summary() {
        // Arrange
        let remote = gitlab_remote();
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
                        "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42\n".to_string(),
                    ))
                })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let review_request = adapter
            .create_authenticated_review_request(remote, input)
            .await
            .expect("GitLab create should succeed");

        // Assert
        assert_eq!(review_request.display_id, "!42");
        assert_eq!(review_request.forge_kind, ForgeKind::GitLab);
        assert_eq!(
            review_request.web_url,
            "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
        );
    }

    #[tokio::test]
    async fn authenticated_review_request_metadata_loads_current_merge_request() {
        // Arrange
        let remote = gitlab_remote();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let metadata = adapter
            .authenticated_review_request_metadata(remote, "!42".to_string())
            .await
            .expect("GitLab metadata lookup should succeed");

        // Assert
        assert_eq!(
            metadata,
            ReviewRequestMetadata {
                body: "Current description.".to_string(),
                title: "Add forge review support".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn sync_authenticated_review_request_metadata_updates_changed_merge_request() {
        // Arrange
        let remote = gitlab_remote();
        let input = UpdateReviewRequestInput {
            body: Some(reconciled_field(
                "Current description.",
                "Updated description.",
            )),
            title: Some(reconciled_field(
                "Add forge review support",
                "Refine forge review support",
            )),
        };
        let edit = ReviewRequestMetadataEdit {
            body: Some("Updated description.".to_string()),
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

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();
                let edit = edit.clone();

                move |command| command == &update_metadata_command(&remote, "42", &edit)
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
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let summary = adapter
            .sync_authenticated_review_request_metadata(remote, "!42".to_string(), input)
            .await
            .expect("GitLab metadata sync should succeed");

        // Assert
        assert_eq!(summary.display_id, "!42");
    }

    #[tokio::test]
    async fn sync_authenticated_review_request_metadata_skips_update_when_unchanged() {
        // Arrange
        let remote = gitlab_remote();
        let input = UpdateReviewRequestInput {
            body: Some(reconciled_field(
                "Current description.",
                "Current description.",
            )),
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

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &view_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let summary = adapter
            .sync_authenticated_review_request_metadata(remote, "!42".to_string(), input)
            .await
            .expect("GitLab metadata sync should succeed");

        // Assert
        assert_eq!(summary.display_id, "!42");
    }

    #[test]
    fn update_metadata_command_can_update_only_title() {
        // Arrange
        let remote = gitlab_remote();
        let edit = ReviewRequestMetadataEdit {
            body: None,
            title: Some("Manual-safe title".to_string()),
        };

        // Act
        let command = update_metadata_command(&remote, "42", &edit);

        // Assert
        assert_eq!(
            command,
            gitlab_command(
                &remote,
                "glab",
                vec![
                    "mr".to_string(),
                    "update".to_string(),
                    "42".to_string(),
                    "--repo".to_string(),
                    remote.web_url.clone(),
                    "--title".to_string(),
                    "Manual-safe title".to_string(),
                    "--yes".to_string(),
                ],
            )
        );
    }

    #[test]
    fn update_metadata_command_can_update_only_description() {
        // Arrange
        let remote = gitlab_remote();
        let edit = ReviewRequestMetadataEdit {
            body: Some("Manual-safe description".to_string()),
            title: None,
        };

        // Act
        let command = update_metadata_command(&remote, "42", &edit);

        // Assert
        assert_eq!(
            command,
            gitlab_command(
                &remote,
                "glab",
                vec![
                    "mr".to_string(),
                    "update".to_string(),
                    "42".to_string(),
                    "--repo".to_string(),
                    remote.web_url.clone(),
                    "--description".to_string(),
                    "Manual-safe description".to_string(),
                    "--yes".to_string(),
                ],
            )
        );
    }

    #[tokio::test]
    async fn list_authenticated_requested_reviews_builds_list_command_and_returns_rows() {
        // Arrange
        let remote = gitlab_remote();
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &requested_reviews_command(&remote)
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_requested_reviews_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let requested_reviews = adapter
            .list_authenticated_requested_reviews(remote)
            .await
            .expect("GitLab requested reviews should load");

        // Assert
        assert_eq!(
            requested_reviews,
            vec![RequestedReview {
                audience: RequestedReviewAudience::Personal,
                author: "octocat".to_string(),
                body: Some("Implements the GitLab provider.".to_string()),
                comment_snapshot: None,
                display_id: "!42".to_string(),
                forge_kind: ForgeKind::GitLab,
                repository: "agentty-xyz/agentty".to_string(),
                status_summary: Some("Draft".to_string()),
                title: "Add forge review support".to_string(),
                updated_at: Some("2026-04-27T21:30:00Z".to_string()),
                web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
            }]
        );
    }

    #[test]
    fn parse_requested_reviews_response_defaults_optional_fields() {
        // Arrange
        let remote = gitlab_remote();
        let stdout = r#"[{
            "iid": 43,
            "title": "Review parser defaults",
            "web_url": "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/43"
        }]"#;

        // Act
        let requested_reviews = parse_requested_reviews_response(stdout, &remote)
            .expect("requested-review defaults should parse");

        // Assert
        assert_eq!(requested_reviews.len(), 1);
        let requested_review = &requested_reviews[0];
        assert_eq!(requested_review.author, "unknown");
        assert_eq!(requested_review.body, None);
        assert_eq!(requested_review.status_summary, None);
        assert_eq!(requested_review.updated_at, None);
    }

    #[test]
    fn gitlab_view_response_maps_terminal_states() {
        // Arrange
        let cases = [
            (
                Some("2026-07-16T12:00:00Z"),
                "opened",
                ReviewRequestState::Merged,
            ),
            (None, "merged", ReviewRequestState::Merged),
            (None, "closed", ReviewRequestState::Closed),
            (None, "locked", ReviewRequestState::Closed),
            (None, "opened", ReviewRequestState::Open),
        ];

        // Act & Assert
        for (merged_at, state, expected) in cases {
            let response = GitLabViewResponse {
                draft: false,
                detailed_merge_status: None,
                iid: 42,
                merge_status: None,
                merged_at: merged_at.map(str::to_string),
                source_branch: "feature/forge".to_string(),
                state: state.to_string(),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
            };

            assert_eq!(response.review_request_state(), expected);
        }
    }

    #[test]
    fn gitlab_merge_status_summary_maps_provider_labels() {
        // Arrange
        let cases = [
            (Some("can_be_merged"), None, Some("Mergeable")),
            (Some("mergeable"), None, Some("Mergeable")),
            (Some("cannot_be_merged"), None, Some("Conflicts")),
            (Some("cannot_be_merged_recheck"), None, Some("Checking")),
            (Some("checking"), None, Some("Checking")),
            (Some("unchecked"), None, Some("Checking")),
            (Some("ci_still_running"), None, Some("Checks pending")),
            (Some("commits_status"), None, Some("Checks pending")),
            (Some("ci_must_pass"), None, Some("Checks required")),
            (
                Some("discussions_not_resolved"),
                None,
                Some("Discussions unresolved"),
            ),
            (Some("draft_status"), None, None),
            (Some("not_open"), None, None),
            (Some("needs_rebase"), None, Some("Needs rebase")),
            (
                Some("can_be_merged"),
                Some("cannot_be_merged"),
                Some("Conflicts"),
            ),
            (None, None, None),
        ];

        // Act & Assert
        for (merge_status, detailed_merge_status, expected) in cases {
            assert_eq!(
                GitLabViewResponse::merge_status_summary(merge_status, detailed_merge_status)
                    .as_deref(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn fetch_authenticated_review_comment_snapshot_parses_discussions_response() {
        // Arrange
        let remote = gitlab_remote();
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &discussions_command(&remote, "42")
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_discussions_json())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let snapshot = adapter
            .fetch_authenticated_review_comment_snapshot(remote, "!42".to_string())
            .await
            .expect("GitLab discussion snapshot should parse");

        // Assert
        assert_eq!(snapshot.threads.len(), 1);
        let thread = &snapshot.threads[0];
        assert_eq!(thread.id, "discussion-1");
        assert_eq!(thread.path, "src/main.rs");
        assert_eq!(thread.line, Some(12));
        assert_eq!(thread.anchor_side, ReviewCommentAnchorSide::New);
        assert_eq!(thread.is_outdated, None);
        assert!(!thread.is_resolved);
        assert_eq!(thread.comments.len(), 2);
        assert_eq!(thread.comments[0].author, "alice");
        assert_eq!(thread.comments[0].body, "Please simplify this.");

        assert_eq!(snapshot.pr_level_comments.len(), 1);
        assert_eq!(snapshot.pr_level_comments[0].author, "carol");
    }

    #[tokio::test]
    async fn review_thread_reply_and_resolution_run_discussion_requests() {
        // Arrange
        let remote = gitlab_remote();
        let thread_id = "discussion/1";
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| {
                    command == &reply_to_thread_command(&remote, "42", "discussion/1", "Addressed.")
                }
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf({
                let remote = remote.clone();

                move |command| command == &resolve_thread_command(&remote, "42", "discussion/1")
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let reply_result = adapter
            .reply_to_authenticated_thread(
                remote.clone(),
                "!42".to_string(),
                thread_id.to_string(),
                "Addressed.".to_string(),
            )
            .await;
        let resolution_result = adapter
            .resolve_authenticated_thread(remote, "!42".to_string(), thread_id.to_string())
            .await;

        // Assert
        assert_eq!(reply_result, Ok(()));
        assert_eq!(resolution_result, Ok(()));
        let encoded_endpoint =
            discussion_endpoint(&gitlab_remote(), "42", "discussion/1", Some("notes"));
        assert!(encoded_endpoint.contains("discussion%2F1/notes"));
    }

    #[test]
    fn detect_remote_supports_gitlab_hosts() {
        // Arrange
        let repo_url = "https://gitlab.com/agentty-xyz/agentty.git";

        // Act
        let remote =
            GitLabReviewRequestAdapter::detect_remote(repo_url).expect("gitlab remote expected");

        // Assert
        assert_eq!(remote.forge_kind, ForgeKind::GitLab);
        assert_eq!(remote.host, "gitlab.com");
        assert_eq!(remote.project_path(), "agentty-xyz/agentty");
    }

    #[test]
    fn parse_display_id_rejects_invalid_merge_request_reference() {
        // Arrange
        let display_id = "!not-a-number";

        // Act
        let error = parse_display_id(display_id).expect_err("invalid display id should fail");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitLab,
                message: "invalid GitLab merge-request display id: `!not-a-number`".to_string(),
            }
        );
    }

    #[test]
    fn parse_create_display_id_reads_merge_request_iid_from_created_url() {
        // Arrange
        let stdout = "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42\n";

        // Act
        let display_id = parse_create_display_id(stdout).expect("create output should parse");

        // Assert
        assert_eq!(display_id, "!42");
    }

    #[test]
    fn parse_create_display_id_rejects_non_numeric_iid() {
        // Arrange
        let stdout = "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/not-a-number\n";

        // Act
        let error = parse_create_display_id(stdout).expect_err("non-numeric iid should fail");

        // Assert
        assert_eq!(
            error,
            "invalid GitLab merge-request display id: `!not-a-number`"
        );
    }

    #[test]
    fn create_command_uses_remote_working_directory_for_glab_git_context() {
        // Arrange
        let remote =
            gitlab_remote().with_command_working_directory(PathBuf::from("/tmp/session-worktree"));
        let input = CreateReviewRequestInput {
            body: Some("Implements the provider adapters.".to_string()),
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
        };

        // Act
        let command = create_command(&remote, &input);

        // Assert
        assert_eq!(
            command.working_directory,
            Some(PathBuf::from("/tmp/session-worktree"))
        );
        assert!(
            command
                .environment
                .contains(&("GITLAB_HOST".to_string(), "gitlab.com".to_string()))
        );
    }

    #[test]
    fn discussions_command_requests_all_gitlab_pages() {
        // Arrange
        let remote = gitlab_remote();

        // Act
        let command = discussions_command(&remote, "42");

        // Assert
        assert!(
            command
                .arguments
                .iter()
                .any(|argument| argument == "--paginate")
        );
        assert!(command.arguments.iter().any(|argument| {
            argument == "/projects/agentty-xyz%2Fagentty/merge_requests/42/discussions?per_page=100"
        }));
    }

    /// Builds one normalized GitLab remote for command-construction tests.
    fn gitlab_remote() -> ForgeRemote {
        ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitLab,
            host: "gitlab.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://gitlab.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
        }
    }

    /// Builds one successful command output with `stdout`.
    fn success_output(stdout: String) -> ForgeCommandOutput {
        ForgeCommandOutput {
            exit_code: Some(0),
            stderr: String::new(),
            stdout,
        }
    }

    /// Returns one representative GitLab merge-request JSON response.
    fn gitlab_view_json() -> String {
        serde_json::json!({
            "description": "Current description.",
            "detailed_merge_status": "can_be_merged",
            "draft": true,
            "iid": 42,
            "merge_status": "can_be_merged",
            "merged_at": null,
            "source_branch": "feature/forge",
            "state": "opened",
            "target_branch": "main",
            "title": "Add forge review support",
            "web_url": "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
        })
        .to_string()
    }

    fn reconciled_field(current: &str, desired: &str) -> ReviewRequestMetadataFieldUpdate {
        ReviewRequestMetadataFieldUpdate {
            current: current.to_string(),
            desired: desired.to_string(),
        }
    }

    /// Returns one `glab mr list --output json` fixture for requested reviews.
    fn gitlab_requested_reviews_json() -> String {
        r#"[
            {
                "draft": true,
                "description": "Implements the GitLab provider.",
                "author": {"name": "Octo Cat", "username": "octocat"},
                "iid": 42,
                "title": "Add forge review support",
                "updated_at": "2026-04-27T21:30:00Z",
                "web_url": "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
            }
        ]"#
        .to_string()
    }

    /// Returns one representative GitLab discussions API response.
    fn gitlab_discussions_json() -> String {
        r#"[
            {
                "id": "discussion-1",
                "individual_note": false,
                "notes": [
                    {
                        "id": 1,
                        "type": "DiffNote",
                        "body": "Please simplify this.",
                        "author": {"name": "Alice", "username": "alice"},
                        "system": false,
                        "resolved": false,
                        "position": {
                            "old_path": "src/main.rs",
                            "new_path": "src/main.rs",
                            "old_line": null,
                            "new_line": 12
                        }
                    },
                    {
                        "id": 2,
                        "type": "DiscussionNote",
                        "body": "Agreed.",
                        "author": {"name": "Bob", "username": "bob"},
                        "system": false,
                        "resolved": false,
                        "position": null
                    }
                ]
            },
            {
                "id": "discussion-2",
                "individual_note": true,
                "notes": [
                    {
                        "id": 3,
                        "type": "DiscussionNote",
                        "body": "Looks good overall.",
                        "author": {"name": "Carol", "username": "carol"},
                        "system": false,
                        "resolved": false,
                        "position": null
                    }
                ]
            }
        ]"#
        .to_string()
    }
}
