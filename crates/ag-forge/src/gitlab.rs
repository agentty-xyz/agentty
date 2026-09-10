//! GitLab review-request adapter routed through the `glab` CLI.

use std::sync::Arc;

use serde::Deserialize;
use url::{Url, form_urlencoded};

use super::{
    CreateReviewRequestInput, ForgeCommand, ForgeCommandRunner, ForgeFuture, ForgeKind,
    ForgeRemote, ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot,
    ReviewCommentThread, ReviewRequestAdapter, ReviewRequestError, ReviewRequestMetadata,
    ReviewRequestMetadataEdit, ReviewRequestOperations, ReviewRequestState, ReviewRequestSummary,
    SyncReviewRequestMetadataConfig, UpdateReviewRequestInput, is_gitlab_host, map_parse_error,
    normalize_provider_label, parse_remote_url, status_summary_parts, strip_port,
};

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

    /// Builds the `glab auth status` command for one GitLab host.
    fn auth_status_command(remote: &ForgeRemote) -> ForgeCommand {
        Self::gitlab_command(
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

    /// Builds the `glab mr list` command for open merge requests matching
    /// `source_branch`.
    fn lookup_command(remote: &ForgeRemote, source_branch: &str) -> ForgeCommand {
        Self::gitlab_command(
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

    /// Parses one optional display id from a GitLab merge-request lookup
    /// response.
    fn parse_lookup_display_id(stdout: &str) -> Result<Option<String>, String> {
        let merge_requests: Vec<GitLabLookupResponse> = serde_json::from_str(stdout)
            .map_err(|error| format!("invalid GitLab merge-request lookup response: {error}"))?;

        Ok(merge_requests
            .first()
            .map(|merge_request| format!("!{}", merge_request.iid)))
    }

    /// Builds the `glab mr create` command for `input`.
    ///
    /// GitLab merge requests default to draft so session-published review
    /// requests do not appear ready for merge before the user chooses to
    /// mark them ready.
    fn create_command(remote: &ForgeRemote, input: &CreateReviewRequestInput) -> ForgeCommand {
        Self::gitlab_command(
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
        let created_url = Url::parse(created_url).map_err(|error| {
            format!("invalid GitLab merge-request create response URL: {error}")
        })?;
        let path_segments = created_url
            .path_segments()
            .ok_or_else(|| "invalid GitLab merge-request create response URL path".to_string())?
            .collect::<Vec<_>>();
        let merge_request_index = path_segments
            .iter()
            .rposition(|segment| *segment == "merge_requests")
            .ok_or_else(|| {
                "missing merge request path segment in create response URL".to_string()
            })?;
        let merge_request_iid = path_segments
            .get(merge_request_index + 1)
            .ok_or_else(|| "missing merge request iid in create response URL".to_string())?;
        let display_id = format!("!{merge_request_iid}");
        Self::parse_display_id_value(&display_id)?;

        Ok(display_id)
    }

    /// Validates one GitLab merge-request display id and returns its numeric
    /// value.
    fn parse_display_id_value(display_id: &str) -> Result<String, String> {
        let trimmed = display_id.trim().trim_start_matches('!');
        if trimmed.is_empty() || !trimmed.chars().all(|character| character.is_ascii_digit()) {
            return Err(format!(
                "invalid GitLab merge-request display id: `{display_id}`"
            ));
        }

        Ok(trimmed.to_string())
    }

    /// Parses one GitLab merge-request display id into the numeric argument for
    /// `glab`.
    fn parse_display_id(display_id: &str) -> Result<String, ReviewRequestError> {
        Self::parse_display_id_value(display_id).map_err(|message| {
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitLab,
                message,
            }
        })
    }

    /// Builds the `glab mr view` command for one merge-request IID.
    fn view_command(remote: &ForgeRemote, merge_request_iid: &str) -> ForgeCommand {
        Self::gitlab_command(
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

    /// Builds GitLab-specific metadata view and edit configuration.
    fn metadata_sync_config() -> SyncReviewRequestMetadataConfig {
        SyncReviewRequestMetadataConfig {
            edit_metadata_command: Self::update_metadata_command,
            edit_operation: "update merge-request metadata",
            parse_display_id: Self::parse_display_id,
            parse_metadata_response: Self::parse_metadata_response,
            view_metadata_command: Self::view_command,
            view_operation: "view merge-request metadata",
        }
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

        Self::gitlab_command(remote, "glab", arguments)
    }

    /// Parses current merge-request title/description metadata from `glab mr
    /// view` JSON.
    fn parse_metadata_response(stdout: &str) -> Result<ReviewRequestMetadata, String> {
        let metadata: GitLabMetadataResponse = serde_json::from_str(stdout)
            .map_err(|error| format!("invalid GitLab merge-request metadata response: {error}"))?;

        Ok(ReviewRequestMetadata {
            body: metadata.description,
            title: metadata.title,
        })
    }

    /// Builds the GitLab API request for the authenticated user identity.
    fn current_user_command(remote: &ForgeRemote) -> ForgeCommand {
        Self::gitlab_command(
            remote,
            "glab",
            vec![
                "api".to_string(),
                "--hostname".to_string(),
                remote.host.clone(),
                "/user".to_string(),
            ],
        )
    }

    /// Parses the authenticated GitLab user required to identify Agentty
    /// replies.
    fn parse_current_user_response(stdout: &str) -> Result<GitLabCurrentUser, String> {
        serde_json::from_str(stdout)
            .map_err(|error| format!("invalid GitLab current-user response: {error}"))
    }

    /// Builds the `glab api` command for merge-request discussions.
    fn discussions_command(remote: &ForgeRemote, merge_request_iid: &str) -> ForgeCommand {
        let encoded_project_path: String =
            form_urlencoded::byte_serialize(remote.project_path().as_bytes()).collect();
        let endpoint = format!(
            "/projects/{encoded_project_path}/merge_requests/{merge_request_iid}/discussions?\
             per_page=100"
        );

        Self::gitlab_command(
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

    /// Parses merge-request discussions into inline threads and MR-level
    /// comments.
    fn parse_review_comment_snapshot_response(
        stdout: &str,
        current_user_id: u64,
    ) -> Result<ReviewCommentSnapshot, String> {
        let discussions: Vec<GitLabDiscussion> = serde_json::from_str(stdout).map_err(|error| {
            format!("invalid GitLab merge-request discussions response: {error}")
        })?;
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

            if let Some(thread) =
                Self::review_comment_thread_from_discussion(&discussion.id, &notes, current_user_id)
            {
                threads.push(thread);
            } else {
                pr_level_comments.extend(
                    notes
                        .drain(..)
                        .map(|note| Self::review_comment_from_note(note, current_user_id)),
                );
            }
        }

        Ok(ReviewCommentSnapshot {
            pr_level_comments,
            threads,
        })
    }

    /// Converts one GitLab discussion into an inline thread when it carries a
    /// diff note position.
    fn review_comment_thread_from_discussion(
        discussion_id: &str,
        notes: &[GitLabDiscussionNote],
        current_user_id: u64,
    ) -> Option<ReviewCommentThread> {
        let anchor_note = notes.iter().find(|note| {
            note.note_type.as_deref() == Some("DiffNote") && note.position.as_ref().is_some()
        })?;
        let position = anchor_note.position.as_ref()?;
        let (anchor_side, path, line) = Self::gitlab_anchor_from_position(position);

        Some(ReviewCommentThread {
            anchor_side,
            comments: notes
                .iter()
                .cloned()
                .map(|note| Self::review_comment_from_note(note, current_user_id))
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

    /// Converts one GitLab discussion note into the forge-neutral comment
    /// shape.
    fn review_comment_from_note(note: GitLabDiscussionNote, current_user_id: u64) -> ReviewComment {
        let authored_by_current_user = note.author.id == Some(current_user_id);

        ReviewComment {
            author: note
                .author
                .username
                .or(note.author.name)
                .unwrap_or_default(),
            authored_by_current_user,
            body: note.body,
        }
    }

    /// Builds one `glab api` request that replies to a merge-request
    /// discussion.
    fn reply_to_thread_command(
        remote: &ForgeRemote,
        merge_request_iid: &str,
        thread_id: &str,
        body: &str,
    ) -> ForgeCommand {
        let endpoint =
            Self::discussion_endpoint(remote, merge_request_iid, thread_id, Some("notes"));

        Self::gitlab_command(
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
        let encoded_thread_id: String =
            form_urlencoded::byte_serialize(thread_id.as_bytes()).collect();
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

    /// Builds one `glab api` request that resolves a merge-request discussion.
    fn resolve_thread_command(
        remote: &ForgeRemote,
        merge_request_iid: &str,
        thread_id: &str,
    ) -> ForgeCommand {
        let endpoint = Self::discussion_endpoint(remote, merge_request_iid, thread_id, None);

        Self::gitlab_command(
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
}

impl ReviewRequestAdapter for GitLabReviewRequestAdapter {
    fn ensure_authenticated(
        &self,
        remote: &ForgeRemote,
    ) -> ForgeFuture<Result<(), ReviewRequestError>> {
        self.operations
            .ensure_authenticated_future(remote.clone(), Self::auth_status_command)
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
            Self::lookup_command,
            "find merge request",
            Self::parse_lookup_display_id,
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
            let create_command = Self::create_command(&remote, &input);
            let output = operations
                .run_review_command(&remote, create_command, "create merge request")
                .await?;
            let display_id = map_parse_error(
                remote.forge_kind,
                Self::parse_create_display_id(&output.stdout),
            )?;

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
            Self::parse_display_id,
            Self::view_command,
            "refresh merge request",
            Self::parse_view_response,
        )
    }

    /// Loads current merge-request title/description metadata.
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
            &Self::metadata_sync_config(),
            move |remote, display_id| {
                adapter.refresh_authenticated_review_request(remote, display_id)
            },
        )
    }

    /// Fetches merge-request discussions through GitLab's REST API and
    /// normalizes diff notes plus review-request-wide notes for session
    /// review-comment views.
    fn fetch_authenticated_review_comment_snapshot(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewCommentSnapshot, ReviewRequestError>> {
        let operations = self.operations.clone();

        Box::pin(async move {
            let merge_request_iid = Self::parse_display_id(&display_id)?;
            let current_user_output = operations
                .run_review_command(
                    &remote,
                    Self::current_user_command(&remote),
                    "fetch authenticated GitLab user",
                )
                .await?;
            let current_user = map_parse_error(
                ForgeKind::GitLab,
                Self::parse_current_user_response(&current_user_output.stdout),
            )?;
            let discussions_output = operations
                .run_review_command(
                    &remote,
                    Self::discussions_command(&remote, &merge_request_iid),
                    "fetch merge-request discussions",
                )
                .await?;

            map_parse_error(
                ForgeKind::GitLab,
                Self::parse_review_comment_snapshot_response(
                    &discussions_output.stdout,
                    current_user.id,
                ),
            )
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
            let merge_request_iid = Self::parse_display_id(&display_id)?;
            operations
                .run_review_command(
                    &remote,
                    Self::reply_to_thread_command(&remote, &merge_request_iid, &thread_id, &body),
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
            let merge_request_iid = Self::parse_display_id(&display_id)?;
            operations
                .run_review_command(
                    &remote,
                    Self::resolve_thread_command(&remote, &merge_request_iid, &thread_id),
                    "resolve merge-request discussion",
                )
                .await?;

            Ok(())
        })
    }
}

/// Minimal GitLab list payload used to find an existing merge request.
#[derive(Deserialize)]
struct GitLabLookupResponse {
    iid: u64,
}

/// GitLab merge-request JSON payload returned by `glab mr view --output json`.
#[derive(Deserialize)]
struct GitLabViewResponse {
    #[serde(rename = "detailed_merge_status")]
    detailed_merge_status: Option<String>,
    #[serde(default)]
    draft: bool,
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

/// Authenticated GitLab user returned by `GET /user`.
#[derive(Deserialize)]
struct GitLabCurrentUser {
    id: u64,
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
    #[serde(rename = "type")]
    note_type: Option<String>,
    position: Option<GitLabDiscussionPosition>,
    #[serde(default)]
    resolved: bool,
    #[serde(default)]
    system: bool,
}

/// Minimal GitLab note author data shown in session review-comment views.
#[derive(Clone, Deserialize)]
struct GitLabDiscussionAuthor {
    id: Option<u64>,
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
#[path = "gitlab_test.rs"]
mod tests;
