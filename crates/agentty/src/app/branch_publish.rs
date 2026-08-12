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

const REVIEW_REQUEST_REMOTE_NAME: &str = "origin";

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

/// Git and forge identities for the remote that owns review requests.
struct ReviewRequestTarget {
    forge_remote: forge::ForgeRemote,
    git_remote_name: &'static str,
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

    /// Rebuilds the popup title for a different publish action while
    /// preserving the blocked/failed distinction and original message.
    #[cfg(test)]
    pub(crate) fn with_action(self, publish_branch_action: PublishBranchAction) -> Self {
        if self.is_blocked {
            Self::blocked(publish_branch_action, self.message)
        } else {
            Self::failed(publish_branch_action, self.message)
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
        BranchPushRemote::Tracking(branch_publish_session.published_upstream_ref.as_deref()),
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

    let review_request_target = review_request_target(
        branch_publish_session,
        git_client.clone(),
        review_request_client.as_ref(),
    )
    .await?;
    validate_new_review_request_publish(
        branch_publish_session,
        &db,
        git_client.as_ref(),
        review_request_target.git_remote_name,
    )
    .await?;

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
        BranchPushRemote::Named {
            name: review_request_target.git_remote_name,
            published_upstream_ref: branch_publish_session.published_upstream_ref.as_deref(),
        },
    )
    .await?;
    let review_request = create_or_refresh_review_request(
        branch_publish_session,
        &clock,
        &db,
        git_client.clone(),
        review_request_client,
        review_request_target.forge_remote,
        branch_name.clone(),
    )
    .await?;

    Ok(BranchPublishTaskSuccess::PullRequestPublished {
        branch_name,
        review_request,
        upstream_reference,
    })
}

/// Validates that a new review request contains only intended review commits.
///
/// The saved base commit is compared with the freshly fetched remote target
/// before any branch push. This prevents local commits that predate the
/// session from leaking into a review request and rejects branches with no
/// commits above the saved boundary. Forks intentionally preserve the source
/// session's boundary, so inherited source commits remain publishable. Linked
/// review requests already passed this gate on creation and continue through
/// the refresh path.
async fn validate_new_review_request_publish(
    branch_publish_session: &BranchPublishTaskSession,
    db: &db::AppRepositories,
    git_client: &dyn GitClient,
    target_remote_name: &str,
) -> Result<(), BranchPublishTaskFailure> {
    if branch_publish_session.review_request.is_some() {
        return Ok(());
    }

    let base_commit_hash = db
        .sessions()
        .get_session_base_commit_hash(&branch_publish_session.id)
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!("Failed to load the session base commit: {error}"),
            )
        })?
        .ok_or_else(|| {
            BranchPublishTaskFailure::blocked(
                PublishBranchAction::PublishPullRequest,
                "Cannot verify which commits belong to this session because its base commit was \
                 not recorded. Sync this session with `r`. Base recovery succeeds only when the \
                 session has no branch-only commits. If publishing remains blocked, preserve any \
                 session work and create a new session."
                    .to_string(),
            )
        })?;

    git_client
        .fetch_named_remote(
            branch_publish_session.folder.clone(),
            target_remote_name.to_string(),
        )
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!("Failed to fetch the remote target before publishing: {error}"),
            )
        })?;

    let remote_base_ref = format!(
        "{target_remote_name}/{}",
        branch_publish_session.base_branch
    );
    let (base_ahead, base_behind) = git_client
        .get_ref_ahead_behind(
            branch_publish_session.folder.clone(),
            base_commit_hash.clone(),
            remote_base_ref.clone(),
        )
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!(
                    "Failed to compare the session base with `{remote_base_ref}` before \
                     publishing: {error}"
                ),
            )
        })?;

    validate_base_relationship(
        branch_publish_session,
        &remote_base_ref,
        base_ahead,
        base_behind,
    )?;

    let (session_ahead, session_behind) = git_client
        .get_ref_ahead_behind(
            branch_publish_session.folder.clone(),
            "HEAD".to_string(),
            base_commit_hash,
        )
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!("Failed to inspect session commits before publishing: {error}"),
            )
        })?;

    if session_ahead == 0 && session_behind == 0 {
        return Err(BranchPublishTaskFailure::blocked(
            PublishBranchAction::PublishPullRequest,
            "Nothing to publish: this session has no commits after its synchronized base."
                .to_string(),
        ));
    }
    if session_behind > 0 {
        return Err(BranchPublishTaskFailure::blocked(
            PublishBranchAction::PublishPullRequest,
            "Cannot publish this review request because the session branch no longer contains its \
             recorded base commit. Sync the session onto the remote target, then retry."
                .to_string(),
        ));
    }

    Ok(())
}

/// Rejects every saved-base relationship except equality with the remote.
fn validate_base_relationship(
    branch_publish_session: &BranchPublishTaskSession,
    remote_base_ref: &str,
    base_ahead: u32,
    base_behind: u32,
) -> Result<(), BranchPublishTaskFailure> {
    match (base_ahead, base_behind) {
        (0, 0) => Ok(()),
        (ahead, 0) => Err(BranchPublishTaskFailure::blocked(
            PublishBranchAction::PublishPullRequest,
            format!(
                "Cannot publish this review request because the session started from a local `{}` \
                 that contains {ahead} {} not present in `{remote_base_ref}`. Publishing would \
                 include changes that predate the session. Synchronize the base branch, create a \
                 new session, then retry.",
                branch_publish_session.base_branch,
                commit_count_label(ahead),
            ),
        )),
        (0, behind) => Err(BranchPublishTaskFailure::blocked(
            PublishBranchAction::PublishPullRequest,
            format!(
                "Cannot publish this review request because `{remote_base_ref}` advanced by \
                 {behind} {} after the session started. Sync the session onto the remote target, \
                 then retry.",
                commit_count_label(behind),
            ),
        )),
        (ahead, behind) => Err(BranchPublishTaskFailure::blocked(
            PublishBranchAction::PublishPullRequest,
            format!(
                "Cannot publish this review request because the saved session base and \
                 `{remote_base_ref}` diverged ({ahead} local-only, {behind} remote-only commits). \
                 Reconcile the base branch and sync the session, then retry."
            ),
        )),
    }
}

/// Returns the singular or plural label for one commit count.
fn commit_count_label(count: u32) -> &'static str {
    if count == 1 {
        return "commit";
    }

    "commits"
}

/// Selects whether a branch push follows Git tracking configuration or uses
/// one explicit remote.
pub(crate) enum BranchPushRemote<'a> {
    Named {
        name: &'a str,
        published_upstream_ref: Option<&'a str>,
    },
    Tracking(Option<&'a str>),
}

/// Pushes the session branch to the configured remote and persists the
/// resulting upstream reference.
///
/// When `remote_branch_name` is supplied and the session has not previously
/// published to the target remote, a pre-flight `git ls-remote` check blocks
/// the push if the remote branch already exists. Without a caller-supplied
/// branch name, the default session branch name is still pushed explicitly so
/// Git does not reuse an inherited base-branch upstream such as `origin/main`.
/// With [`BranchPushRemote::Named`], both lookup and push are pinned to that
/// remote instead of consulting the branch's tracking configuration.
pub(crate) async fn push_session_branch_to_remote(
    db: &db::AppRepositories,
    folder: PathBuf,
    git_client: Arc<dyn GitClient>,
    publish_branch_action: PublishBranchAction,
    session_id: &str,
    remote_branch_name: Option<&str>,
    branch_push_remote: BranchPushRemote<'_>,
) -> Result<String, BranchPublishTaskFailure> {
    let (published_upstream_ref, target_remote_name) = match branch_push_remote {
        BranchPushRemote::Named {
            name,
            published_upstream_ref,
        } => (published_upstream_ref, Some(name)),
        BranchPushRemote::Tracking(published_upstream_ref) => (published_upstream_ref, None),
    };
    let retry_text = retry_action_text(publish_branch_action);
    let target_branch =
        remote_branch_name.map_or_else(|| session::session_branch(session_id), str::to_string);
    let is_published_to_target = target_remote_name.map_or_else(
        || published_upstream_ref.is_some(),
        |target_remote_name| {
            published_upstream_ref
                .and_then(|upstream_reference| upstream_reference.split_once('/'))
                .is_some_and(|(remote_name, _)| remote_name == target_remote_name)
        },
    );

    ensure_session_branch_push_safe(
        git_client.as_ref(),
        folder.clone(),
        publish_branch_action,
        session_id,
    )
    .await?;

    if let Some(target_branch) = remote_branch_name
        && !is_published_to_target
    {
        ensure_remote_branch_available(
            git_client.as_ref(),
            folder.clone(),
            publish_branch_action,
            retry_text,
            target_branch,
            target_remote_name,
        )
        .await?;
    }

    let push_branch = if let Some(target_remote_name) = target_remote_name {
        git_client.push_current_branch_to_named_remote_branch(
            folder,
            target_remote_name.to_string(),
            target_branch,
        )
    } else {
        git_client.push_current_branch_to_remote_branch(folder, target_branch)
    };
    let upstream_reference = push_branch.await.map_err(|error| {
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

/// Blocks a first custom-branch push when that branch already exists on its
/// selected remote.
async fn ensure_remote_branch_available(
    git_client: &dyn GitClient,
    folder: PathBuf,
    publish_branch_action: PublishBranchAction,
    retry_text: &str,
    target_branch: &str,
    target_remote_name: Option<&str>,
) -> Result<(), BranchPublishTaskFailure> {
    let remote_branch_exists = if let Some(target_remote_name) = target_remote_name {
        git_client.remote_branch_exists_on_named_remote(
            folder,
            target_remote_name.to_string(),
            target_branch.to_string(),
        )
    } else {
        git_client.remote_branch_exists(folder, target_branch.to_string())
    };
    let already_exists = remote_branch_exists.await.map_err(|error| {
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
                "Remote branch `{target_branch}` already exists. Choose a different name or use \
                 the default session branch."
            ),
        ));
    }

    Ok(())
}

/// Resolves the Git and forge identities for review-request publishing.
async fn review_request_target(
    branch_publish_session: &BranchPublishTaskSession,
    git_client: Arc<dyn GitClient>,
    review_request_client: &dyn forge::ReviewRequestClient,
) -> Result<ReviewRequestTarget, BranchPublishTaskFailure> {
    let repo_url = git_client
        .repo_url(branch_publish_session.folder.clone())
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                format!("Failed to resolve repository remote for review request: {error}"),
            )
        })?;

    let forge_remote = review_request_client
        .detect_remote(repo_url)
        .map(|remote| remote.with_command_working_directory(branch_publish_session.folder.clone()))
        .map_err(|error| {
            BranchPublishTaskFailure::failed(
                PublishBranchAction::PublishPullRequest,
                error.detail_message(),
            )
        })?;

    Ok(ReviewRequestTarget {
        forge_remote,
        git_remote_name: REVIEW_REQUEST_REMOTE_NAME,
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

/// Maps one branch-publish failure into blocked or failed popup copy.
#[cfg(test)]
pub(crate) fn branch_push_failure(
    publish_branch_action: PublishBranchAction,
    error: &str,
) -> BranchPublishTaskFailure {
    if !is_git_push_authentication_error(error) {
        return BranchPublishTaskFailure::failed(
            publish_branch_action,
            format!("Failed to publish session branch: {error}"),
        );
    }

    BranchPublishTaskFailure::blocked(
        publish_branch_action,
        git_push_authentication_message(
            detected_forge_kind_from_git_push_error(error),
            match publish_branch_action {
                PublishBranchAction::Push => "push the branch again",
                PublishBranchAction::PublishPullRequest => "publish the review request again",
            },
        ),
    )
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
mod tests {
    use ag_git as git;

    use super::*;
    use crate::infra::db::AppRepositories;

    #[test]
    fn review_request_queued_label_describes_waiting_without_loading_punctuation() {
        // Arrange, Act
        let label = review_request_queued_label();

        // Assert
        assert_eq!(label, "review request — publish after this turn");
    }

    async fn database_with_session_base_commit(base_commit_hash: Option<&str>) -> AppRepositories {
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        if let Some(base_commit_hash) = base_commit_hash {
            database
                .sessions()
                .update_session_base_commit_hash("session-id", base_commit_hash.to_string())
                .await
                .expect("failed to persist session base commit");
        }

        database
    }

    fn unpublished_review_session() -> BranchPublishTaskSession {
        BranchPublishTaskSession {
            base_branch: "main".to_string(),
            folder: PathBuf::from("/tmp/session-worktree"),
            id: "session-id".into(),
            published_upstream_ref: None,
            review_request: None,
            status: Status::Review,
        }
    }

    fn expect_fetched_remote_base_comparison(
        mock_git_client: &mut git::MockGitClient,
        remote_base_ref: &'static str,
        result: Result<(u32, u32), git::GitError>,
    ) {
        mock_git_client
            .expect_fetch_named_remote()
            .once()
            .withf(|_, remote_name| remote_name == REVIEW_REQUEST_REMOTE_NAME)
            .returning(|_, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_get_ref_ahead_behind()
            .once()
            .withf(move |_, left_ref, right_ref| {
                left_ref == "base-commit" && right_ref == remote_base_ref
            })
            .return_once(move |_, _, _| Box::pin(async move { result }));
    }

    fn expect_session_commit_comparison(
        mock_git_client: &mut git::MockGitClient,
        result: Result<(u32, u32), git::GitError>,
    ) {
        mock_git_client
            .expect_get_ref_ahead_behind()
            .once()
            .withf(|_, left_ref, right_ref| left_ref == "HEAD" && right_ref == "base-commit")
            .return_once(move |_, _, _| Box::pin(async move { result }));
    }

    #[tokio::test]
    async fn publish_pull_request_uses_resolved_target_instead_of_prior_upstream() {
        // Arrange
        let database = database_with_session_base_commit(Some("base-commit")).await;
        let mut branch_publish_session = unpublished_review_session();
        branch_publish_session.published_upstream_ref =
            Some("review-remote/feature/review".to_string());
        let expected_branch = "feature/review".to_string();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty/repo.git".to_string()) })
        });
        expect_fetched_remote_base_comparison(&mut mock_git_client, "origin/main", Ok((0, 0)));
        expect_session_commit_comparison(&mut mock_git_client, Ok((2, 0)));
        expect_safe_session_branch_push(&mut mock_git_client, "session-id");
        mock_git_client
            .expect_remote_branch_exists_on_named_remote()
            .once()
            .withf(|_, remote_name, remote_branch_name| {
                remote_name == REVIEW_REQUEST_REMOTE_NAME && remote_branch_name == "feature/review"
            })
            .returning(|_, _, _| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_push_current_branch_to_named_remote_branch()
            .once()
            .withf({
                let expected_branch = expected_branch.clone();

                move |_, remote_name, remote_branch_name| {
                    remote_name == REVIEW_REQUEST_REMOTE_NAME
                        && remote_branch_name == &expected_branch
                }
            })
            .returning(|_, _, _| Box::pin(async { Ok("origin/feature/review".to_string()) }));
        let review_request_summary = forge::ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: forge::ForgeKind::GitHub,
            source_branch: expected_branch.clone(),
            state: forge::ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Session review".to_string(),
            web_url: "https://github.com/agentty/repo/pull/42".to_string(),
        };
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .once()
            .withf(|repo_url| repo_url == "https://github.com/agentty/repo.git")
            .returning(|repo_url| {
                Ok(forge::ForgeRemote {
                    command_working_directory: None,
                    forge_kind: forge::ForgeKind::GitHub,
                    host: "github.com".to_string(),
                    namespace: "agentty".to_string(),
                    project: "repo".to_string(),
                    repo_url,
                    web_url: "https://github.com/agentty/repo".to_string(),
                })
            });
        mock_review_request_client
            .expect_find_by_source_branch()
            .once()
            .withf({
                let expected_branch = expected_branch.clone();

                move |remote, source_branch| {
                    remote.command_working_directory == Some(PathBuf::from("/tmp/session-worktree"))
                        && source_branch == &expected_branch
                }
            })
            .returning({
                let review_request_summary = review_request_summary.clone();

                move |_, _| {
                    let review_request_summary = review_request_summary.clone();

                    Box::pin(async move { Ok(Some(review_request_summary)) })
                }
            });
        mock_review_request_client
            .expect_refresh_review_request()
            .once()
            .withf(|_, display_id| display_id == "#42")
            .return_once({
                let review_request_summary = review_request_summary.clone();

                move |_, _| Box::pin(async move { Ok(review_request_summary) })
            });

        // Act
        let result = publish_pull_request(
            &branch_publish_session,
            database,
            Arc::new(crate::infra::clock::RealClock),
            Arc::new(mock_git_client),
            Arc::new(mock_review_request_client),
            Some(&expected_branch),
        )
        .await;

        // Assert
        assert!(matches!(
            result,
            Ok(BranchPublishTaskSuccess::PullRequestPublished {
                branch_name,
                review_request: ReviewRequest { summary, .. },
                upstream_reference,
            }) if branch_name == expected_branch
                && summary == review_request_summary
                && upstream_reference == "origin/feature/review"
        ));
    }

    #[tokio::test]
    async fn new_review_request_requires_recorded_base_commit() {
        // Arrange
        let database = database_with_session_base_commit(None).await;
        let branch_publish_session = unpublished_review_session();
        let mock_git_client = git::MockGitClient::new();

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        let failure = result.expect_err("missing base metadata should block publication");
        assert_eq!(failure.title, "Review request publish blocked");
        assert!(failure.message.contains("base commit was not recorded"));
        assert!(failure.message.contains("Sync this session with `r`"));
        assert!(failure.message.contains("no branch-only commits"));
        assert!(failure.message.contains("create a new session"));
    }

    #[tokio::test]
    async fn new_review_request_reports_base_commit_lookup_failure() {
        // Arrange
        let (database, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        pool.close().await;
        let branch_publish_session = unpublished_review_session();
        let mock_git_client = git::MockGitClient::new();

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        let failure = result.expect_err("closed database should stop publication");
        assert_eq!(failure.title, "Review request publish failed");
        assert!(
            failure
                .message
                .contains("Failed to load the session base commit")
        );
    }

    #[tokio::test]
    async fn new_review_request_reports_remote_fetch_failure() {
        // Arrange
        let database = database_with_session_base_commit(Some("base-commit")).await;
        let branch_publish_session = unpublished_review_session();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_fetch_named_remote()
            .once()
            .withf(|_, remote_name| remote_name == REVIEW_REQUEST_REMOTE_NAME)
            .returning(|_, _| {
                Box::pin(async {
                    Err(git::GitError::OutputParse("remote unavailable".to_string()))
                })
            });

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        let failure = result.expect_err("fetch failure should stop publication");
        assert_eq!(failure.title, "Review request publish failed");
        assert!(
            failure
                .message
                .contains("Failed to fetch the remote target")
        );
    }

    #[tokio::test]
    async fn new_review_request_blocks_unsynchronized_base_relationships() {
        // Arrange
        let cases = [
            ((1, 0), "contains 1 commit not present"),
            ((6, 0), "contains 6 commits not present"),
            ((0, 2), "advanced by 2 commits"),
            ((2, 3), "diverged (2 local-only, 3 remote-only commits)"),
        ];

        for (relationship, expected_message) in cases {
            let database = database_with_session_base_commit(Some("base-commit")).await;
            let branch_publish_session = unpublished_review_session();
            let mut mock_git_client = git::MockGitClient::new();
            expect_fetched_remote_base_comparison(
                &mut mock_git_client,
                "origin/main",
                Ok(relationship),
            );

            // Act
            let result = validate_new_review_request_publish(
                &branch_publish_session,
                &database,
                &mock_git_client,
                REVIEW_REQUEST_REMOTE_NAME,
            )
            .await;

            // Assert
            let failure = result.expect_err("unsynchronized base should block publication");
            assert_eq!(failure.title, "Review request publish blocked");
            assert!(failure.message.contains(expected_message));
        }
    }

    #[tokio::test]
    async fn new_review_request_reports_base_comparison_failure() {
        // Arrange
        let database = database_with_session_base_commit(Some("base-commit")).await;
        let branch_publish_session = unpublished_review_session();
        let mut mock_git_client = git::MockGitClient::new();
        expect_fetched_remote_base_comparison(
            &mut mock_git_client,
            "origin/main",
            Err(git::GitError::OutputParse("missing target".to_string())),
        );

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        let failure = result.expect_err("comparison failure should stop publication");
        assert_eq!(failure.title, "Review request publish failed");
        assert!(
            failure
                .message
                .contains("Failed to compare the session base")
        );
    }

    #[tokio::test]
    async fn new_review_request_blocks_session_without_commits() {
        // Arrange
        let database = database_with_session_base_commit(Some("base-commit")).await;
        let branch_publish_session = unpublished_review_session();
        let mut mock_git_client = git::MockGitClient::new();
        expect_fetched_remote_base_comparison(&mut mock_git_client, "origin/main", Ok((0, 0)));
        expect_session_commit_comparison(&mut mock_git_client, Ok((0, 0)));

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        let failure = result.expect_err("unchanged session should not publish");
        assert_eq!(failure.title, "Review request publish blocked");
        assert!(failure.message.contains("Nothing to publish"));
    }

    #[tokio::test]
    async fn new_review_request_blocks_session_that_lost_its_base() {
        // Arrange
        let database = database_with_session_base_commit(Some("base-commit")).await;
        let branch_publish_session = unpublished_review_session();
        let mut mock_git_client = git::MockGitClient::new();
        expect_fetched_remote_base_comparison(&mut mock_git_client, "origin/main", Ok((0, 0)));
        expect_session_commit_comparison(&mut mock_git_client, Ok((2, 1)));

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        let failure = result.expect_err("rewritten session base should block publication");
        assert_eq!(failure.title, "Review request publish blocked");
        assert!(
            failure
                .message
                .contains("no longer contains its recorded base commit")
        );
    }

    #[tokio::test]
    async fn new_review_request_reports_session_comparison_failure() {
        // Arrange
        let database = database_with_session_base_commit(Some("base-commit")).await;
        let branch_publish_session = unpublished_review_session();
        let mut mock_git_client = git::MockGitClient::new();
        expect_fetched_remote_base_comparison(&mut mock_git_client, "origin/main", Ok((0, 0)));
        expect_session_commit_comparison(
            &mut mock_git_client,
            Err(git::GitError::OutputParse("invalid head".to_string())),
        );

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        let failure = result.expect_err("session comparison failure should stop publication");
        assert_eq!(failure.title, "Review request publish failed");
        assert!(
            failure
                .message
                .contains("Failed to inspect session commits")
        );
    }

    #[tokio::test]
    async fn new_review_request_uses_review_target_instead_of_prior_upstream_remote() {
        // Arrange
        let database = database_with_session_base_commit(Some("base-commit")).await;
        let mut branch_publish_session = unpublished_review_session();
        branch_publish_session.published_upstream_ref =
            Some("review-remote/wt/session-id".to_string());
        let mut mock_git_client = git::MockGitClient::new();
        expect_fetched_remote_base_comparison(&mut mock_git_client, "origin/main", Ok((0, 0)));
        expect_session_commit_comparison(&mut mock_git_client, Ok((2, 0)));

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn forked_review_request_accepts_inherited_source_and_followup_commits() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("source-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert source session");
        database
            .sessions()
            .update_session_base_commit_hash("source-id", "base-commit".to_string())
            .await
            .expect("failed to persist source base commit");
        database
            .sessions()
            .fork_session_snapshot(db::ForkSessionSnapshot {
                new_session_id: "session-id",
                source_session_id: "source-id",
                status: "Review",
            })
            .await
            .expect("failed to fork source session");
        let branch_publish_session = unpublished_review_session();
        let mut mock_git_client = git::MockGitClient::new();
        expect_fetched_remote_base_comparison(&mut mock_git_client, "origin/main", Ok((0, 0)));
        expect_session_commit_comparison(&mut mock_git_client, Ok((3, 0)));

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn linked_review_request_skips_creation_safety_gate() {
        // Arrange
        let database = database_with_session_base_commit(None).await;
        let mut branch_publish_session = unpublished_review_session();
        branch_publish_session.review_request = Some(ReviewRequest {
            last_refreshed_at: 1,
            summary: forge::ReviewRequestSummary {
                display_id: "#1".to_string(),
                forge_kind: forge::ForgeKind::GitHub,
                source_branch: "wt/session-id".to_string(),
                state: forge::ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Existing review".to_string(),
                web_url: "https://github.com/agentty/repo/pull/1".to_string(),
            },
        });
        let mock_git_client = git::MockGitClient::new();

        // Act
        let result = validate_new_review_request_publish(
            &branch_publish_session,
            &database,
            &mock_git_client,
            REVIEW_REQUEST_REMOTE_NAME,
        )
        .await;

        // Assert
        assert!(result.is_ok());
    }

    fn expect_safe_session_branch_push(mock_git_client: &mut git::MockGitClient, session_id: &str) {
        let expected_branch = session::session_branch(session_id);
        mock_git_client
            .expect_in_progress_operation()
            .once()
            .returning(|_| Box::pin(async { Ok(None) }));
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(move |_| {
                let expected_branch = expected_branch.clone();

                Box::pin(async move { Some(expected_branch) })
            });
    }

    async fn push_session_branch_to_remote_with_mock(
        mock_git_client: git::MockGitClient,
    ) -> Result<String, BranchPublishTaskFailure> {
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");

        push_session_branch_to_remote(
            &database,
            PathBuf::from("/tmp/session-worktree"),
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            None,
            BranchPushRemote::Tracking(Some("origin/wt/session-id")),
        )
        .await
    }

    #[tokio::test]
    async fn review_request_target_binds_origin_to_detected_forge_remote() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let branch_publish_session = BranchPublishTaskSession {
            base_branch: "main".to_string(),
            folder: session_folder.clone(),
            id: "session-id".into(),
            published_upstream_ref: None,
            review_request: None,
            status: Status::Review,
        };
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_repo_url()
            .once()
            .withf({
                let session_folder = session_folder.clone();
                move |candidate_folder| candidate_folder == &session_folder
            })
            .returning(|_| {
                Box::pin(async { Ok("https://gitlab.com/agentty-xyz/agentty.git".to_string()) })
            });
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .once()
            .withf(|repo_url| repo_url == "https://gitlab.com/agentty-xyz/agentty.git")
            .returning(|_| {
                Ok(forge::ForgeRemote {
                    command_working_directory: None,
                    forge_kind: forge::ForgeKind::GitLab,
                    host: "gitlab.com".to_string(),
                    namespace: "agentty-xyz".to_string(),
                    project: "agentty".to_string(),
                    repo_url: "https://gitlab.com/agentty-xyz/agentty.git".to_string(),
                    web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
                })
            });

        // Act
        let target = review_request_target(
            &branch_publish_session,
            Arc::new(mock_git_client),
            &mock_review_request_client,
        )
        .await
        .expect("remote should resolve");

        // Assert
        assert_eq!(target.git_remote_name, "origin");
        assert_eq!(
            target.forge_remote.command_working_directory,
            Some(session_folder)
        );
        assert_eq!(target.forge_remote.forge_kind, forge::ForgeKind::GitLab);
    }

    #[tokio::test]
    async fn review_request_target_reports_forge_detection_failure() {
        // Arrange
        let branch_publish_session = unpublished_review_session();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("ssh://unsupported.example/repo.git".to_string()) })
        });
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .once()
            .returning(|repo_url| Err(forge::ReviewRequestError::UnsupportedRemote { repo_url }));

        // Act
        let result = review_request_target(
            &branch_publish_session,
            Arc::new(mock_git_client),
            &mock_review_request_client,
        )
        .await;

        // Assert
        let failure = result.err().expect("unsupported forge remote should fail");
        assert_eq!(failure.title, "Review request publish failed");
        assert!(
            failure
                .message
                .contains("only supported for GitHub and GitLab")
        );
    }

    #[tokio::test]
    async fn push_session_branch_to_remote_persists_upstream_reference() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let expected_session_folder = session_folder.clone();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_remote_branch_exists()
            .once()
            .returning(|_, _| Box::pin(async { Ok(false) }));
        expect_safe_session_branch_push(&mut mock_git_client, "session-id");
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(move |folder, remote_branch_name| {
                folder == &expected_session_folder && remote_branch_name == "wt/session-id"
            })
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));

        // Act
        let upstream_reference = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            Some("wt/session-id"),
            BranchPushRemote::Tracking(None),
        )
        .await
        .expect("branch push should succeed");
        let persisted_session = database
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions")
            .into_iter()
            .find(|session| session.id == "session-id")
            .expect("missing session row");

        // Assert
        assert_eq!(upstream_reference, "origin/wt/session-id");
        assert_eq!(
            persisted_session.published_upstream_ref.as_deref(),
            Some("origin/wt/session-id")
        );
    }

    /// Describes one auth-guidance parsing scenario for `branch_push_failure`.
    struct AuthGuidanceCase {
        error: &'static str,
        expected_cli_guidance: Option<&'static str>,
        name: &'static str,
    }

    /// Verifies branch-push auth guidance uses detected forge hints when the
    /// error text includes a recognizable host.
    #[test]
    fn branch_push_failure_uses_detected_forge_guidance() {
        // Arrange
        let error =
            "git push failed: could not read username for 'https://github.com/openai/agentty': \
             terminal prompts disabled";

        // Act
        let failure = branch_push_failure(PublishBranchAction::Push, error);

        // Assert
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("gh auth login"));
        assert!(failure.message.contains("push the branch again"));
    }

    /// Verifies auth guidance handles additional git push error formats
    /// without regressing forge detection or fallback messaging.
    #[test]
    fn branch_push_failure_handles_multiple_auth_error_formats() {
        // Arrange
        let cases = vec![
            AuthGuidanceCase {
                name: "mixed-case https url",
                error: "Git push failed: fatal: could not read Username for 'HTTPS://GitHub.com/OpenAI/agentty': terminal prompts disabled",
                expected_cli_guidance: Some("gh auth login"),
            },
            AuthGuidanceCase {
                name: "password prompt without scheme",
                error: "Git push failed: fatal: could not read Password for 'github.com/OpenAI/agentty': terminal prompts disabled",
                expected_cli_guidance: Some("gh auth login"),
            },
            AuthGuidanceCase {
                name: "github url with port and subpath",
                error: "Git push failed: fatal: could not read Username for 'https://user@github.com:443/openai/agentty/path': terminal prompts disabled",
                expected_cli_guidance: Some("gh auth login"),
            },
            AuthGuidanceCase {
                name: "gitlab host uses glab guidance",
                error: "Git push failed: fatal: could not read Username for 'https://gitlab.com/openai/agentty': terminal prompts disabled",
                expected_cli_guidance: Some("glab auth login"),
            },
            AuthGuidanceCase {
                name: "self-hosted gitlab token uses glab guidance",
                error: "Git push failed: authentication failed while contacting gitlab.company.org for review branch",
                expected_cli_guidance: Some("glab auth login"),
            },
            AuthGuidanceCase {
                name: "non-forge host falls back to generic guidance",
                error: "Git push failed: fatal: could not read Username for 'https://example.com/openai/agentty': terminal prompts disabled",
                expected_cli_guidance: None,
            },
        ];

        // Act
        for case in cases {
            let failure = branch_push_failure(PublishBranchAction::Push, case.error);

            // Assert
            assert_eq!(failure.title, "Branch push blocked", "case: {}", case.name);
            assert!(
                failure.message.contains("push the branch again"),
                "case: {}",
                case.name
            );
            if let Some(expected_cli_guidance) = case.expected_cli_guidance {
                assert!(
                    failure.message.contains(expected_cli_guidance),
                    "case: {}",
                    case.name
                );
            } else {
                assert!(
                    !failure.message.contains("gh auth login"),
                    "case: {}",
                    case.name
                );
                assert!(
                    !failure.message.contains("glab auth login"),
                    "case: {}",
                    case.name
                );
                assert!(
                    failure.message.contains("PAT/SSH key or credential helper"),
                    "case: {}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn branch_push_success_message_uses_gitlab_merge_request_copy() {
        // Arrange
        let review_request_creation = ReviewRequestCreationInfo {
            forge_kind: forge::ForgeKind::GitLab,
            web_url: Some(
                "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/new".to_string(),
            ),
        };

        // Act
        let message = branch_push_success_message("wt/session-1", Some(&review_request_creation));

        // Assert
        assert!(message.contains("create the merge request"));
        assert!(message.contains("gitlab.com/agentty-xyz/agentty/-/merge_requests/new"));
    }

    #[test]
    fn review_request_created_notice_uses_gitlab_short_name() {
        // Arrange
        let review_request = ReviewRequest {
            last_refreshed_at: 42,
            summary: forge::ReviewRequestSummary {
                display_id: "!24".to_string(),
                forge_kind: forge::ForgeKind::GitLab,
                source_branch: "wt/session-1".to_string(),
                state: forge::ReviewRequestState::Open,
                status_summary: Some("Draft".to_string()),
                target_branch: "main".to_string(),
                title: "Add GitLab support".to_string(),
                web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24".to_string(),
            },
        };

        // Act
        let notice = review_request_created_notice(&review_request);

        // Assert
        assert_eq!(
            notice,
            "\n[Review Request] Created MR \
             https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24\n"
        );
    }

    #[tokio::test]
    async fn push_checks_custom_branch_on_explicit_target_remote() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let mut mock_git_client = git::MockGitClient::new();
        expect_safe_session_branch_push(&mut mock_git_client, "session-id");
        mock_git_client
            .expect_remote_branch_exists_on_named_remote()
            .once()
            .withf(|_, remote_name, branch_name| {
                remote_name == REVIEW_REQUEST_REMOTE_NAME && branch_name == "feature/existing"
            })
            .returning(|_, _, _| Box::pin(async { Ok(true) }));

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            Some("feature/existing"),
            BranchPushRemote::Named {
                name: REVIEW_REQUEST_REMOTE_NAME,
                published_upstream_ref: None,
            },
        )
        .await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("already exists"));
    }

    #[tokio::test]
    async fn push_reports_named_remote_branch_lookup_failure() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let mut mock_git_client = git::MockGitClient::new();
        expect_safe_session_branch_push(&mut mock_git_client, "session-id");
        mock_git_client
            .expect_remote_branch_exists_on_named_remote()
            .once()
            .returning(|_, _, _| {
                Box::pin(async {
                    Err(git::GitError::OutputParse(
                        "remote lookup failed".to_string(),
                    ))
                })
            });

        // Act
        let result = push_session_branch_to_remote(
            &database,
            PathBuf::from("/tmp/session-worktree"),
            Arc::new(mock_git_client),
            PublishBranchAction::PublishPullRequest,
            "session-id",
            Some("feature/review"),
            BranchPushRemote::Named {
                name: REVIEW_REQUEST_REMOTE_NAME,
                published_upstream_ref: None,
            },
        )
        .await;

        // Assert
        let failure = result.expect_err("named remote lookup should fail");
        assert_eq!(failure.title, "Review request publish failed");
        assert!(
            failure
                .message
                .contains("Failed to check remote branch existence")
        );
    }

    #[tokio::test]
    async fn push_skips_existence_check_when_upstream_ref_already_set() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let expected_folder = session_folder.clone();
        let mut mock_git_client = git::MockGitClient::new();
        expect_safe_session_branch_push(&mut mock_git_client, "session-id");
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(move |folder, branch| folder == &expected_folder && branch == "feature/existing")
            .returning(|_, _| Box::pin(async { Ok("origin/feature/existing".to_string()) }));

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            Some("feature/existing"),
            BranchPushRemote::Tracking(Some("origin/feature/existing")),
        )
        .await;

        // Assert
        let upstream = result.expect("push should succeed");
        assert_eq!(upstream, "origin/feature/existing");
    }

    #[tokio::test]
    async fn push_skips_existence_check_when_no_custom_branch_name() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let expected_folder = session_folder.clone();
        let expected_branch = session::session_branch("session-id");
        let expected_upstream = format!("origin/{expected_branch}");
        let expected_upstream_assertion = expected_upstream.clone();
        let mut mock_git_client = git::MockGitClient::new();
        expect_safe_session_branch_push(&mut mock_git_client, "session-id");
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(move |folder, branch| folder == &expected_folder && branch == &expected_branch)
            .returning(move |_, _| {
                let expected_upstream = expected_upstream.clone();

                Box::pin(async move { Ok(expected_upstream) })
            });

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            None,
            BranchPushRemote::Tracking(None),
        )
        .await;

        // Assert
        let upstream = result.expect("push should succeed");
        assert_eq!(upstream, expected_upstream_assertion);
    }

    #[tokio::test]
    async fn push_blocks_while_session_branch_rebase_is_in_progress() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_in_progress_operation()
            .once()
            .returning(|_| Box::pin(async { Ok(Some(git::InProgressGitOperation::Rebase)) }));
        mock_git_client.expect_detect_git_info().times(0);
        mock_git_client.expect_remote_branch_exists().times(0);
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .times(0);

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            Some("feature/new-branch"),
            BranchPushRemote::Tracking(None),
        )
        .await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("rebase is in progress"));
    }

    #[tokio::test]
    async fn push_blocks_while_session_branch_merge_is_in_progress() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_in_progress_operation()
            .once()
            .returning(|_| Box::pin(async { Ok(Some(git::InProgressGitOperation::Merge)) }));
        mock_git_client.expect_detect_git_info().times(0);
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .times(0);

        // Act
        let result = push_session_branch_to_remote_with_mock(mock_git_client).await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("merge is in progress"));
    }

    #[tokio::test]
    async fn push_blocks_while_session_branch_cherry_pick_is_in_progress() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_in_progress_operation()
            .once()
            .returning(|_| Box::pin(async { Ok(Some(git::InProgressGitOperation::CherryPick)) }));
        mock_git_client.expect_detect_git_info().times(0);
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .times(0);

        // Act
        let result = push_session_branch_to_remote_with_mock(mock_git_client).await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("cherry-pick is in progress"));
    }

    #[tokio::test]
    async fn push_blocks_while_session_branch_revert_is_in_progress() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_in_progress_operation()
            .once()
            .returning(|_| Box::pin(async { Ok(Some(git::InProgressGitOperation::Revert)) }));
        mock_git_client.expect_detect_git_info().times(0);
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .times(0);

        // Act
        let result = push_session_branch_to_remote_with_mock(mock_git_client).await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("revert is in progress"));
    }

    #[tokio::test]
    async fn push_blocks_when_worktree_is_not_on_session_branch() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_in_progress_operation()
            .once()
            .returning(|_| Box::pin(async { Ok(None) }));
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("main".to_string()) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .times(0);

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            None,
            BranchPushRemote::Tracking(Some("origin/wt/session-id")),
        )
        .await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("worktree is on `main`"));
        assert!(failure.message.contains("instead of `wt/session-`"));
    }

    #[tokio::test]
    async fn push_reports_folder_when_session_branch_cannot_be_detected() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_in_progress_operation()
            .once()
            .returning(|_| Box::pin(async { Ok(None) }));
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { None }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .times(0);

        // Act
        let result = push_session_branch_to_remote_with_mock(mock_git_client).await;

        // Assert
        let failure = result.expect_err("push should fail");
        assert_eq!(failure.title, "Branch push failed");
        assert!(
            failure
                .message
                .contains("`/tmp/session-worktree` before pushing")
        );
    }

    #[tokio::test]
    async fn push_shows_auth_guidance_when_ls_remote_returns_auth_error() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let mut mock_git_client = git::MockGitClient::new();
        expect_safe_session_branch_push(&mut mock_git_client, "session-id");
        mock_git_client
            .expect_remote_branch_exists()
            .once()
            .returning(|_, _| {
                Box::pin(async move {
                    Err(git::GitError::CommandFailed {
                        command: "git ls-remote".to_string(),
                        stderr: "fatal: could not read Username for \
                                 'https://github.com/org/repo': terminal prompts disabled"
                            .to_string(),
                    })
                })
            });

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            Some("feature/new-branch"),
            BranchPushRemote::Tracking(None),
        )
        .await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("Git push requires authentication"));
        assert!(failure.message.contains("push the branch again"));
        assert!(failure.message.contains("gh auth login"));
    }

    #[tokio::test]
    async fn push_shows_auth_guidance_when_push_returns_auth_error() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let session_folder = PathBuf::from("/tmp/session-worktree");
        let expected_folder = session_folder.clone();
        let expected_branch = session::session_branch("session-id");
        let expected_push_command = format!("git push origin HEAD:{expected_branch}");
        let mut mock_git_client = git::MockGitClient::new();
        expect_safe_session_branch_push(&mut mock_git_client, "session-id");
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(move |folder, branch| folder == &expected_folder && branch == &expected_branch)
            .returning(move |_, _| {
                let expected_push_command = expected_push_command.clone();

                Box::pin(async {
                    Err(git::GitError::CommandFailed {
                        command: expected_push_command,
                        stderr:
                            "fatal: Authentication failed for 'https://gitlab.com/org/repo.git/'"
                                .to_string(),
                    })
                })
            });

        // Act
        let result = push_session_branch_to_remote(
            &database,
            session_folder,
            Arc::new(mock_git_client),
            PublishBranchAction::Push,
            "session-id",
            None,
            BranchPushRemote::Tracking(None),
        )
        .await;

        // Assert
        let failure = result.expect_err("push should be blocked");
        assert_eq!(failure.title, "Branch push blocked");
        assert!(failure.message.contains("Git push requires authentication"));
        assert!(failure.message.contains("push the branch again"));
        assert!(failure.message.contains("glab auth login"));
    }

    #[test]
    fn with_action_preserves_blocked_distinction() {
        // Arrange
        let blocked =
            BranchPublishTaskFailure::blocked(PublishBranchAction::Push, "auth error".to_string());
        let failed = BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "generic error".to_string(),
        );

        // Act
        let adjusted_blocked = blocked.with_action(PublishBranchAction::PublishPullRequest);
        let adjusted_failed = failed.with_action(PublishBranchAction::PublishPullRequest);

        // Assert
        assert_eq!(adjusted_blocked.title, "Review request publish blocked");
        assert_eq!(adjusted_blocked.message, "auth error");
        assert_eq!(adjusted_failed.title, "Review request publish failed");
        assert_eq!(adjusted_failed.message, "generic error");
    }
}
