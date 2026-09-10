//! Branch-publish workflow helpers for session review branches.

use std::path::PathBuf;
use std::sync::Arc;

use ag_forge as forge;
use ag_git::GitClient;

use super::session::{self, unix_timestamp_from_system_time};
use crate::app::review_request;
use crate::domain::session::{PublishBranchAction, ReviewRequest, Session, SessionId, Status};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::clock::Clock;
use crate::infra::db;

/// Session snapshot cloned into a branch-publish background task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BranchPublishTaskSession {
    /// Review-request target branch used when a forge link is generated after
    /// push.
    pub(crate) base_branch: String,
    /// Session worktree used for git push and remote inspection.
    pub(crate) folder: PathBuf,
    /// Stable session identifier.
    pub(crate) id: SessionId,
    /// Persisted upstream reference from a previous push, when the session
    /// already tracks one.
    pub(crate) published_upstream_ref: Option<String>,
    /// Persisted linked review request, when the session already tracks one.
    pub(crate) review_request: Option<ReviewRequest>,
    /// Current session lifecycle state checked before push.
    pub(crate) status: Status,
}

impl BranchPublishTaskSession {
    /// Builds one background-task snapshot from a live session row.
    ///
    /// The app layer may override `base_branch` with a stacked parent publish
    /// target before moving this snapshot into the background task.
    pub(crate) fn from_session(session: &Session) -> Self {
        Self {
            base_branch: session.base_branch.clone(),
            folder: session.folder.clone(),
            id: session.id.clone(),
            published_upstream_ref: session.published_upstream_ref.clone(),
            review_request: session.review_request.clone(),
            status: session.status,
        }
    }
}

/// Session snapshot and shared operation lock moved into one publish task.
pub(crate) struct BranchPublishTaskContext {
    /// Serializes manual publishing with other branch mutations for the same
    /// session.
    pub(crate) branch_operation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Immutable session data used throughout the background workflow.
    pub(crate) session: BranchPublishTaskSession,
}

/// Final reducer payload for a completed branch-publish background action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BranchPublishActionUpdate {
    /// Branch-publish task result routed through the reducer.
    pub(crate) result: BranchPublishTaskResult,
    /// Session id targeted by the completed action.
    pub(crate) session_id: SessionId,
}

/// Error payload shown inline in session chat for branch-publish failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BranchPublishTaskFailure {
    /// Whether the failure represents a blocked state (e.g. auth required)
    /// rather than an execution error.
    pub(crate) is_blocked: bool,
    /// Inline body text describing the failure.
    pub(crate) message: String,
    /// Inline title shown for the failure.
    pub(crate) title: String,
}

impl BranchPublishTaskFailure {
    /// Builds one blocked-state popup payload from an actionable message.
    pub(crate) fn blocked(publish_branch_action: PublishBranchAction, message: String) -> Self {
        Self {
            is_blocked: true,
            message,
            title: match publish_branch_action {
                PublishBranchAction::Push => "Branch push blocked".to_string(),
                PublishBranchAction::PublishPullRequest => {
                    "Review request publish blocked".to_string()
                }
            },
        }
    }

    /// Builds one failure-state popup payload from an execution error.
    pub(crate) fn failed(publish_branch_action: PublishBranchAction, message: String) -> Self {
        Self {
            is_blocked: false,
            message,
            title: match publish_branch_action {
                PublishBranchAction::Push => "Branch push failed".to_string(),
                PublishBranchAction::PublishPullRequest => {
                    "Review request publish failed".to_string()
                }
            },
        }
    }
}

/// Successful outcome returned by a branch-publish background action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BranchPublishTaskSuccess {
    /// Carries the pushed branch name and persisted upstream reference.
    Pushed {
        /// Remote branch name that was pushed successfully.
        branch_name: String,
        /// Optional forge-native metadata that can open or describe the new
        /// review-request flow.
        review_request_creation: Option<ReviewRequestCreationInfo>,
        /// Persisted upstream ref recorded after the successful push.
        upstream_reference: String,
    },
    /// Carries the pushed branch name, linked review request, and upstream
    /// ref.
    PullRequestPublished {
        /// Remote branch name that was pushed successfully.
        branch_name: String,
        /// Persisted review-request summary refreshed or created by the action.
        review_request: ReviewRequest,
        /// Persisted upstream ref recorded after the successful push.
        upstream_reference: String,
    },
}

/// Reducer-friendly result for a completed branch-publish background action.
pub(crate) type BranchPublishTaskResult =
    Result<BranchPublishTaskSuccess, BranchPublishTaskFailure>;

/// Extracts the review request created by a completed publish action.
///
/// # Errors
/// Returns the user-facing publish failure, or an invariant error when a
/// plain branch-push result is supplied for review-request creation.
pub(crate) fn review_request_from_publish_result(
    result: &BranchPublishTaskResult,
) -> Result<ReviewRequest, String> {
    match result {
        Ok(BranchPublishTaskSuccess::PullRequestPublished { review_request, .. }) => {
            Ok(review_request.clone())
        }
        Ok(BranchPublishTaskSuccess::Pushed { .. }) => {
            Err("Review-request publishing completed without a review request".to_string())
        }
        Err(BranchPublishTaskFailure { message, .. }) => Err(message.clone()),
    }
}

/// Forge-specific metadata used to describe one review-request creation path
/// after a branch push.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewRequestCreationInfo {
    /// Forge family that can open or create the follow-up review request.
    pub(crate) forge_kind: forge::ForgeKind,
    /// Optional forge-native URL for starting the review-request flow.
    pub(crate) web_url: Option<String>,
}

/// Returns the inline loading label for one branch-publish action.
pub(crate) fn branch_publish_loading_label(publish_branch_action: PublishBranchAction) -> String {
    match publish_branch_action {
        PublishBranchAction::Push => "Pushing branch...".to_string(),
        PublishBranchAction::PublishPullRequest => "Publishing review request...".to_string(),
    }
}

/// Returns the inline label shown while review-request creation waits behind
/// an active session turn.
pub(crate) fn review_request_queued_label() -> String {
    "review request — publish after this turn".to_string()
}

/// Returns the inline success title for a completed branch-publish action.
pub(crate) fn branch_publish_success_title(publish_branch_action: PublishBranchAction) -> String {
    match publish_branch_action {
        PublishBranchAction::Push => "Branch pushed".to_string(),
        PublishBranchAction::PublishPullRequest => "Review request published".to_string(),
    }
}

/// Returns the success popup body for one completed branch push.
pub(crate) fn branch_push_success_message(
    branch_name: &str,
    review_request_creation: Option<&ReviewRequestCreationInfo>,
) -> String {
    match review_request_creation {
        Some(ReviewRequestCreationInfo {
            forge_kind,
            web_url: Some(review_request_creation_url),
        }) => format!(
            "Pushed session branch `{branch_name}`.\n\nOpen this link to create the {}:\n{}",
            forge_kind.review_request_name(),
            review_request_creation_url
        ),
        Some(ReviewRequestCreationInfo {
            forge_kind,
            web_url: None,
        }) => format!(
            "Pushed session branch `{branch_name}`.\n\nCreate the {} manually from your forge UI.",
            forge_kind.review_request_name()
        ),
        None => format!(
            "Pushed session branch `{branch_name}`.\n\nCreate the review request manually from \
             your forge UI."
        ),
    }
}

/// Returns the durable transcript notice for one completed review-request
/// publish.
pub(crate) fn review_request_created_notice(review_request: &ReviewRequest) -> String {
    TranscriptNotice::ReviewRequest.format(format!(
        "Created {} {}",
        review_request
            .summary
            .forge_kind
            .review_request_short_name(),
        review_request.summary.web_url
    ))
}

/// Executes one background branch-publish action while holding the session's
/// shared branch-operation lock.
pub(crate) async fn run_branch_publish_action(
    publish_branch_action: PublishBranchAction,
    branch_publish_context: BranchPublishTaskContext,
    db: db::AppRepositories,
    clock: Arc<dyn Clock>,
    git_client: Arc<dyn GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    remote_branch_name: Option<String>,
) -> BranchPublishTaskResult {
    let BranchPublishTaskContext {
        branch_operation_lock,
        session: branch_publish_session,
    } = branch_publish_context;
    let _branch_operation_guard = branch_operation_lock.lock_owned().await;

    match publish_branch_action {
        PublishBranchAction::Push => {
            push_session_branch(
                publish_branch_action,
                &branch_publish_session,
                db,
                git_client,
                remote_branch_name.as_deref(),
            )
            .await
        }
        PublishBranchAction::PublishPullRequest => {
            publish_pull_request(
                &branch_publish_session,
                db,
                clock,
                git_client,
                review_request_client,
                remote_branch_name.as_deref(),
            )
            .await
        }
    }
}

/// Returns whether error output looks like a git push authentication failure.
/// Returns whether `normalized_detail` (already lower-cased) contains any
/// credential- or authentication-related keywords produced by git remote
/// operations.
fn has_authentication_error_keywords(normalized_detail: &str) -> bool {
    normalized_detail.contains("authentication failed")
        || normalized_detail.contains("terminal prompts disabled")
        || normalized_detail.contains("could not read username")
        || normalized_detail.contains("could not read password")
        || normalized_detail.contains("permission denied")
        || normalized_detail.contains("access denied")
        || normalized_detail.contains("not authorized")
        || normalized_detail.contains("support for password authentication was removed")
        || normalized_detail.contains("the requested url returned error: 403")
        || normalized_detail.contains("repository not found")
}

pub(crate) fn is_git_push_authentication_error(detail_message: &str) -> bool {
    let normalized_detail = detail_message.to_ascii_lowercase();

    let is_push_context = normalized_detail.contains("git push failed")
        || (normalized_detail.contains("push")
            && (normalized_detail.contains("remote") || normalized_detail.contains("origin")));
    if !is_push_context {
        return false;
    }

    has_authentication_error_keywords(&normalized_detail)
}

/// Attempts to infer one forge kind from a git push authentication failure.
pub(crate) fn detected_forge_kind_from_git_push_error(
    detail_message: &str,
) -> Option<forge::ForgeKind> {
    let normalized_detail = detail_message.to_ascii_lowercase();

    if let Some(forge_kind) = detected_forge_kind_from_push_auth_url(&normalized_detail) {
        return Some(forge_kind);
    }

    if let Some(forge_kind) = detected_forge_kind_from_text(detail_message) {
        return Some(forge_kind);
    }

    if normalized_detail.contains(" gh ") {
        return Some(forge::ForgeKind::GitHub);
    }

    if normalized_detail.contains(" glab ") {
        return Some(forge::ForgeKind::GitLab);
    }

    None
}

/// Returns the user-facing retry guidance phrase for one publish action.
fn retry_action_text(publish_branch_action: PublishBranchAction) -> &'static str {
    match publish_branch_action {
        PublishBranchAction::Push => "push the branch again",
        PublishBranchAction::PublishPullRequest => "publish the review request again",
    }
}

/// Returns actionable copy for one git push authentication failure.
pub(crate) fn git_push_authentication_message(
    forge_kind: Option<forge::ForgeKind>,
    retry_action: &str,
) -> String {
    match forge_kind {
        Some(forge::ForgeKind::GitHub) => format!(
            "Git push requires authentication for this repository.\nAuthorize git access, then \
             {retry_action}.\nRun `gh auth login`, or configure credentials with a PAT/SSH key."
        ),
        Some(forge::ForgeKind::GitLab) => format!(
            "Git push requires authentication for this repository.\nAuthorize git access, then \
             {retry_action}.\nRun `glab auth login`, or configure credentials with a PAT/SSH key."
        ),
        None => format!(
            "Git push requires authentication for this repository.\nAuthorize git access, then \
             {retry_action}.\nConfigure Git credentials with a PAT/SSH key or credential helper."
        ),
    }
}

/// Pushes one session branch to the configured Git remote.
pub(crate) async fn push_session_branch(
    publish_branch_action: PublishBranchAction,
    branch_publish_session: &BranchPublishTaskSession,
    db: db::AppRepositories,
    git_client: Arc<dyn GitClient>,
    remote_branch_name: Option<&str>,
) -> BranchPublishTaskResult {
    if !branch_publish_session.status.allows_review_actions() {
        return Err(BranchPublishTaskFailure::failed(
            publish_branch_action,
            "Session must be in review to push the branch.".to_string(),
        ));
    }

    let branch_name = remote_branch_name.map_or_else(
        || session::session_branch(&branch_publish_session.id),
        str::to_string,
    );
    let upstream_reference = push_session_branch_to_remote(
        &db,
        branch_publish_session.folder.clone(),
        git_client.clone(),
        publish_branch_action,
        &branch_publish_session.id,
        remote_branch_name,
        branch_publish_session.published_upstream_ref.as_deref(),
    )
    .await?;
    let review_request_creation =
        branch_review_request_creation_info(branch_publish_session, git_client, &branch_name).await;

    Ok(BranchPublishTaskSuccess::Pushed {
        branch_name,
        review_request_creation,
        upstream_reference,
    })
}

/// Pushes one session branch, then creates or refreshes its forge review
/// request.
async fn publish_pull_request(
    branch_publish_session: &BranchPublishTaskSession,
    db: db::AppRepositories,
    clock: Arc<dyn Clock>,
    git_client: Arc<dyn GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    remote_branch_name: Option<&str>,
) -> BranchPublishTaskResult {
    if !branch_publish_session.status.allows_review_actions() {
        return Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::PublishPullRequest,
            "Session must be in review to publish the review request.".to_string(),
        ));
    }

    let branch_name = remote_branch_name.map_or_else(
        || session::session_branch(&branch_publish_session.id),
        str::to_string,
    );
    let upstream_reference = push_session_branch_to_remote(
        &db,
        branch_publish_session.folder.clone(),
        git_client.clone(),
        PublishBranchAction::PublishPullRequest,
        &branch_publish_session.id,
        remote_branch_name,
        branch_publish_session.published_upstream_ref.as_deref(),
    )
    .await?;
    let remote = review_request_remote(
        branch_publish_session,
        git_client.clone(),
        review_request_client.as_ref(),
    )
    .await?;
    let review_request = create_or_refresh_review_request(
        branch_publish_session,
        &clock,
        &db,
        git_client.clone(),
        review_request_client,
        remote,
        branch_name.clone(),
    )
    .await?;

    Ok(BranchPublishTaskSuccess::PullRequestPublished {
        branch_name,
        review_request,
        upstream_reference,
    })
}

/// Pushes the session branch to the configured remote and persists the
/// resulting upstream reference.
///
/// When `remote_branch_name` is supplied and the session has no prior
/// `published_upstream_ref`, a pre-flight remote lookup blocks the push when
/// that branch currently exists. When it does not exist, an explicit empty
/// lease allows recreation despite stale local remote-tracking refs while
/// still refusing a concurrent remote creation. Without a caller-supplied
/// branch name, the default session branch name is still pushed explicitly so
/// Git does not reuse an inherited base-branch upstream such as `origin/main`.
pub(crate) async fn push_session_branch_to_remote(
    db: &db::AppRepositories,
    folder: PathBuf,
    git_client: Arc<dyn GitClient>,
    publish_branch_action: PublishBranchAction,
    session_id: &str,
    remote_branch_name: Option<&str>,
    published_upstream_ref: Option<&str>,
) -> Result<String, BranchPublishTaskFailure> {
    let retry_text = retry_action_text(publish_branch_action);
    let target_branch =
        remote_branch_name.map_or_else(|| session::session_branch(session_id), str::to_string);

    ensure_session_branch_push_safe(
        git_client.as_ref(),
        folder.clone(),
        publish_branch_action,
        session_id,
    )
    .await?;

    if let Some(target_branch) = remote_branch_name
        && published_upstream_ref.is_none()
    {
        let already_exists = git_client
            .remote_branch_exists(folder.clone(), target_branch.to_string())
            .await
            .map_err(|error| {
                let detail = error.to_string();
                let normalized = detail.to_ascii_lowercase();

                if has_authentication_error_keywords(&normalized) {
                    BranchPublishTaskFailure::blocked(
                        publish_branch_action,
                        git_push_authentication_message(
                            detected_forge_kind_from_git_push_error(&detail),
                            retry_text,
                        ),
                    )
                } else {
                    BranchPublishTaskFailure::failed(
                        publish_branch_action,
                        format!("Failed to check remote branch existence: {error}"),
                    )
                }
            })?;

        if already_exists {
            return Err(BranchPublishTaskFailure::blocked(
                publish_branch_action,
                format!(
                    "Remote branch `{target_branch}` already exists. Choose a different name or \
                     use the default session branch."
                ),
            ));
        }
    }

    let push = if remote_branch_name.is_some() && published_upstream_ref.is_none() {
        git_client.push_current_branch_to_new_remote_branch(folder, target_branch)
    } else {
        git_client.push_current_branch_to_remote_branch(folder, target_branch)
    };
    let upstream_reference = push.await.map_err(|error| {
        let detail = error.to_string();
        let normalized = detail.to_ascii_lowercase();

        if has_authentication_error_keywords(&normalized) {
            BranchPublishTaskFailure::blocked(
                publish_branch_action,
                git_push_authentication_message(
                    detected_forge_kind_from_git_push_error(&detail),
                    retry_text,
                ),
            )
        } else {
            BranchPublishTaskFailure::failed(
                publish_branch_action,
                format!("Failed to publish session branch: {error}"),
            )
        }
    })?;

    db.sessions()
        .update_session_published_upstream_ref(session_id, Some(upstream_reference.clone()))
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                publish_branch_action,
                format!(
                    "Branch push succeeded, but Agentty could not persist the upstream reference: \
                     {error}"
                ),
            )
        })?;

    Ok(upstream_reference)
}

/// Blocks session-branch force-pushes while the worktree is in an unsafe git
/// state.
async fn ensure_session_branch_push_safe(
    git_client: &dyn GitClient,
    folder: PathBuf,
    publish_branch_action: PublishBranchAction,
    session_id: &str,
) -> Result<(), BranchPublishTaskFailure> {
    let retry_text = retry_action_text(publish_branch_action);
    let folder_display = folder.display().to_string();
    let in_progress_operation = git_client
        .in_progress_operation(folder.clone())
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                publish_branch_action,
                format!(
                    "Failed to inspect session branch git state in `{folder_display}`: {error}"
                ),
            )
        })?;
    if let Some(in_progress_operation) = in_progress_operation {
        return Err(BranchPublishTaskFailure::blocked(
            publish_branch_action,
            format!(
                "Session branch push is paused because {} is in progress in `{folder_display}`. \
                 Finish or abort the {}, then {retry_text}.",
                in_progress_operation.article_name(),
                in_progress_operation.name()
            ),
        ));
    }

    let expected_branch = session::session_branch(session_id);
    let Some(current_branch) = git_client.detect_git_info(folder).await else {
        return Err(BranchPublishTaskFailure::failed(
            publish_branch_action,
            format!(
                "Failed to detect the current session branch in `{folder_display}` before pushing."
            ),
        ));
    };
    if current_branch != expected_branch {
        return Err(BranchPublishTaskFailure::blocked(
            publish_branch_action,
            format!(
                "Refusing to push session branch because the worktree is on `{current_branch}` \
                 instead of `{expected_branch}`. Return to the session branch, then {retry_text}."
            ),
        ));
    }

    Ok(())
}

/// Resolves one forge remote for review-request publishing.
async fn review_request_remote(
    branch_publish_session: &BranchPublishTaskSession,
    git_client: Arc<dyn GitClient>,
    review_request_client: &dyn forge::ReviewRequestClient,
) -> Result<forge::ForgeRemote, BranchPublishTaskFailure> {
    let repo_url = git_client
        .repo_url(branch_publish_session.folder.clone())
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!("Failed to resolve repository remote for review request: {error}"),
            )
        })?;

    review_request_client
        .detect_remote(repo_url)
        .map(|remote| remote.with_command_working_directory(branch_publish_session.folder.clone()))
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                error.detail_message(),
            )
        })
}

/// Creates or refreshes one review request for the published session branch and
/// persists the normalized summary.
async fn create_or_refresh_review_request(
    branch_publish_session: &BranchPublishTaskSession,
    clock: &Arc<dyn Clock>,
    db: &db::AppRepositories,
    git_client: Arc<dyn GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    remote: forge::ForgeRemote,
    source_branch: String,
) -> Result<ReviewRequest, BranchPublishTaskFailure> {
    let review_request_summary =
        if let Some(review_request) = &branch_publish_session.review_request {
            review_request_client
                .refresh_review_request(remote, review_request.summary.display_id.clone())
                .await
                .map_err(|error| {
                    BranchPublishTaskFailure::failed(
                        PublishBranchAction::PublishPullRequest,
                        error.detail_message(),
                    )
                })?
        } else if let Some(existing_review_request) = review_request_client
            .find_by_source_branch(remote.clone(), source_branch.clone())
            .await
            .map_err(|error| {
                BranchPublishTaskFailure::failed(
                    PublishBranchAction::PublishPullRequest,
                    error.detail_message(),
                )
            })?
        {
            review_request_client
                .refresh_review_request(remote, existing_review_request.display_id)
                .await
                .map_err(|error| {
                    BranchPublishTaskFailure::failed(
                        PublishBranchAction::PublishPullRequest,
                        error.detail_message(),
                    )
                })?
        } else {
            let create_input =
                load_review_request_create_input(branch_publish_session, git_client, source_branch)
                    .await?;

            review_request_client
                .create_review_request(remote, create_input)
                .await
                .map_err(|error| {
                    BranchPublishTaskFailure::failed(
                        PublishBranchAction::PublishPullRequest,
                        error.detail_message(),
                    )
                })?
        };
    let review_request = ReviewRequest {
        last_refreshed_at: unix_timestamp_from_system_time(clock.now_system_time()),
        summary: review_request_summary,
    };

    db.reviews()
        .update_session_review_request(&branch_publish_session.id, Some(review_request.clone()))
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!(
                    "Review-request publish succeeded, but Agentty could not persist the linked \
                     review request: {error}"
                ),
            )
        })?;

    Ok(review_request)
}

/// Builds one normalized create-request payload from branch-publish session
/// commit message.
async fn load_review_request_create_input(
    branch_publish_session: &BranchPublishTaskSession,
    git_client: Arc<dyn GitClient>,
    source_branch: String,
) -> Result<forge::CreateReviewRequestInput, BranchPublishTaskFailure> {
    let commit_message = git_client
        .head_commit_message(branch_publish_session.folder.clone())
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!("Failed to load session branch commit message: {error}"),
            )
        })?
        .ok_or_else(|| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                "Session branch has no commit message for review-request publishing.".to_string(),
            )
        })?;
    let review_request_commit_message =
        review_request::parse_review_request_commit_message(&commit_message).ok_or_else(|| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                "Session branch commit message must have a non-empty title for review-request \
                 publishing."
                    .to_string(),
            )
        })?;

    Ok(forge::CreateReviewRequestInput {
        body: review_request_commit_message.body,
        source_branch,
        target_branch: branch_publish_session.base_branch.clone(),
        title: review_request_commit_message.title,
    })
}

/// Returns one forge-native review-request creation helper for a pushed
/// session.
async fn branch_review_request_creation_info(
    branch_publish_session: &BranchPublishTaskSession,
    git_client: Arc<dyn GitClient>,
    branch_name: &str,
) -> Option<ReviewRequestCreationInfo> {
    let repo_url = git_client
        .repo_url(branch_publish_session.folder.clone())
        .await
        .ok()?;
    let remote = forge::detect_remote(&repo_url).ok()?;

    Some(ReviewRequestCreationInfo {
        forge_kind: remote.forge_kind,
        web_url: remote
            .review_request_creation_url(branch_name, &branch_publish_session.base_branch)
            .ok(),
    })
}

/// Returns one forge family from the remote host shown in a credential error.
fn detected_forge_kind_from_push_auth_url(detail_message: &str) -> Option<forge::ForgeKind> {
    let host = extract_push_auth_prompt_host(detail_message)?;
    if host.is_empty() {
        return None;
    }

    let host = strip_port(host);
    if is_github_host(host) {
        return Some(forge::ForgeKind::GitHub);
    }

    if forge::is_gitlab_host(host) {
        return Some(forge::ForgeKind::GitLab);
    }

    None
}

/// Returns whether `host` is a GitHub-style forge host.
fn is_github_host(host: &str) -> bool {
    host == "github.com" || host.ends_with(".github.com")
}

/// Attempts to infer one forge kind from host-like tokens inside free-form
/// git push error text.
fn detected_forge_kind_from_text(detail_message: &str) -> Option<forge::ForgeKind> {
    for token in detail_message.split_whitespace() {
        let normalized_host = normalized_host_token(token);
        if normalized_host.is_empty() {
            continue;
        }

        if is_github_host(normalized_host) {
            return Some(forge::ForgeKind::GitHub);
        }

        if forge::is_gitlab_host(normalized_host) {
            return Some(forge::ForgeKind::GitLab);
        }
    }

    None
}

/// Normalizes one host-like token found in free-form error text so forge
/// detection can inspect just the hostname.
fn normalized_host_token(token: &str) -> &str {
    let token = token
        .trim()
        .trim_matches(|character: char| "\"'`()[]{}<>,;:".contains(character));
    let token = token
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("ssh://");
    let token = token.rsplit_once('@').map_or(token, |(_, host)| host);
    let token = token.split('/').next().unwrap_or(token);

    strip_port(token)
}

/// Extracts one remote host from one `git push` authentication prompt.
fn extract_push_auth_prompt_host(detail_message: &str) -> Option<&str> {
    let username_marker = "could not read username for '";
    let password_marker = "could not read password for '";

    if let Some(host) = extract_host_from_prompt(detail_message, username_marker) {
        return Some(host);
    }

    extract_host_from_prompt(detail_message, password_marker)
}

/// Extracts the host payload from one quoted credential-prompt URL.
fn extract_host_from_prompt<'detail>(
    detail_message: &'detail str,
    marker: &str,
) -> Option<&'detail str> {
    let marker_start = detail_message.find(marker)?;
    let quoted_host = &detail_message[marker_start + marker.len()..];
    let host = quoted_host.split('\'').next()?;
    let host = host.trim().trim_end_matches('/');
    let host = host
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = host.split('/').next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);

    Some(host)
}

/// Removes one explicit host port, if present.
fn strip_port(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

#[cfg(test)]
#[path = "branch_publish_test.rs"]
pub(crate) mod tests;
