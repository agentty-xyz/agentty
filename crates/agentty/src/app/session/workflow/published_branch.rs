//! Published-branch post-turn synchronization for session workers.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use ag_agent::OneShotClient;
use ag_forge as forge;
use ag_git::GitClient;
use tokio::sync::{OwnedMutexGuard, mpsc};
use uuid::Uuid;

use super::SessionTaskService;
use crate::app::session::{
    Clock, SessionError, remote_branch_name_from_upstream_ref, unix_timestamp_from_system_time,
};
use crate::app::{AppEvent, branch_publish};
use crate::domain::agent::AgentSelection;
use crate::domain::session::{
    PublishBranchAction, PublishedBranchSyncStatus, ReviewRequest, ReviewRequestState, SessionId,
};
use crate::domain::session_message::SessionTranscript;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::db::{AppRepositories, SessionReviewCommentResolutionRow};

/// Prefix for the per-operation marker appended to forge replies.
const REVIEW_COMMENT_REPLY_MARKER_PREFIX: &str = "<!-- agentty review resolution:";

/// Result of ensuring one durable operation has an Agentty-authored reply.
enum ReviewCommentReplyProgress {
    /// The durable operation proves the reply was posted.
    Recorded,
    /// The thread became resolved before Agentty could post its reply.
    ThreadResolved,
    /// Reply progress could not be proven or safely advanced.
    Unavailable,
}

/// Owned inputs required to start one detached published-branch auto-push.
pub(super) struct PublishedBranchAutoPushStartInput {
    /// Reducer event sender used to publish auto-push progress and completion.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Per-session guard retained until the detached push finishes.
    pub(super) branch_operation_guard: OwnedMutexGuard<()>,
    /// Clock used to timestamp optional review-request metadata refresh.
    pub(super) clock: Arc<dyn Clock>,
    /// Repository bundle used to resolve and persist branch-publish state.
    pub(super) db: AppRepositories,
    /// Session worktree folder pushed to its tracked upstream branch.
    pub(super) folder: PathBuf,
    /// Git boundary used for the remote push operation.
    pub(super) git_client: Arc<dyn GitClient>,
    /// Provider-neutral one-shot boundary used for metadata reconciliation.
    pub(super) one_shot_client: Arc<dyn OneShotClient>,
    /// Published upstream reference that provides the remote branch target.
    pub(super) published_upstream_ref: String,
    /// Forge boundary used for optional linked PR/MR metadata refresh.
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Optional auto-commit message used to refresh linked PR/MR metadata.
    pub(super) review_request_commit_message: Option<String>,
    /// Agent/model selection used for metadata reconciliation.
    pub(super) session_agent: AgentSelection,
    /// Session id whose branch is being pushed.
    pub(super) session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: crate::app::service::SessionUpdateVersionMap,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

/// Starts one detached auto-push task for a session that already tracks a
/// published upstream branch.
pub(super) fn start_published_branch_auto_push(input: PublishedBranchAutoPushStartInput) {
    let branch_operation_guard = input.branch_operation_guard;
    let sync_operation_id = Uuid::new_v4().to_string();
    let review_request_metadata_sync =
        input
            .review_request_commit_message
            .map(|commit_message| ReviewRequestMetadataSyncInput {
                clock: Arc::clone(&input.clock),
                commit_message: Some(commit_message),
                evaluation: ReviewRequestMetadataEvaluationInput {
                    one_shot_client: Arc::clone(&input.one_shot_client),
                    session_agent: input.session_agent,
                },
                review_request_client: Arc::clone(&input.review_request_client),
            });
    let _ = input
        .app_event_tx
        .send(AppEvent::PublishedBranchSyncUpdated {
            persistent_notice: None,
            session_id: input.session_id.clone(),
            sync_operation_id: sync_operation_id.clone(),
            sync_status: PublishedBranchSyncStatus::InProgress,
        });

    let auto_push_input = PublishedBranchAutoPushInput {
        app_event_tx: input.app_event_tx,
        db: input.db,
        folder: input.folder,
        git_client: input.git_client,
        published_upstream_ref: input.published_upstream_ref,
        review_request_client: input.review_request_client,
        review_request_metadata_sync,
        session_id: input.session_id,
        session_update_versions: input.session_update_versions,
        sync_operation_id,
        transcript: input.transcript,
    };
    tokio::spawn(async move {
        let _branch_operation_guard = branch_operation_guard;
        run_published_branch_auto_push_task(auto_push_input).await;
    });
}

/// Owned inputs needed by one detached published-branch auto-push task across
/// session workflows.
pub(super) struct PublishedBranchAutoPushInput {
    /// Reducer event sender used to publish auto-push progress and completion.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Repository bundle used to resolve and persist branch-publish state.
    pub(super) db: AppRepositories,
    /// Session worktree folder pushed to its tracked upstream branch.
    pub(super) folder: PathBuf,
    /// Git boundary used for the remote push operation.
    pub(super) git_client: Arc<dyn GitClient>,
    /// Published upstream reference that provides the remote branch target.
    pub(super) published_upstream_ref: String,
    /// Forge boundary used for durable review-comment resolution operations.
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Optional metadata sync payload used after a successful post-turn push.
    pub(super) review_request_metadata_sync: Option<ReviewRequestMetadataSyncInput>,
    /// Session id whose branch is being pushed.
    pub(super) session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: crate::app::service::SessionUpdateVersionMap,
    /// Auto-push operation id used to ignore stale completion updates.
    pub(super) sync_operation_id: String,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

/// Owned dependencies for one optional linked PR/MR metadata sync after push.
pub(super) struct ReviewRequestMetadataSyncInput {
    /// Clock used to timestamp the refreshed review-request summary.
    pub(super) clock: Arc<dyn Clock>,
    /// Known auto-commit message, or `None` to resolve it after the push.
    pub(super) commit_message: Option<String>,
    /// Semantic evaluator used for completed session turns.
    pub(super) evaluation: ReviewRequestMetadataEvaluationInput,
    /// Forge boundary used to refresh linked PR/MR metadata after a push.
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
}

/// All-or-nothing inputs for semantic review-request metadata reconciliation.
pub(super) struct ReviewRequestMetadataEvaluationInput {
    /// Provider-neutral one-shot boundary used for semantic reconciliation.
    pub(super) one_shot_client: Arc<dyn OneShotClient>,
    /// Agent/model selection used for semantic reconciliation.
    pub(super) session_agent: AgentSelection,
}

/// Runs one detached auto-push for a previously published session branch and
/// reports its state through the app event pipeline.
pub(super) async fn run_published_branch_auto_push(input: PublishedBranchAutoPushInput) {
    run_published_branch_auto_push_task(input).await;
}

/// Executes one detached published-branch auto-push from owned task inputs.
async fn run_published_branch_auto_push_task(input: PublishedBranchAutoPushInput) {
    let remote_branch_name = remote_branch_name_from_upstream_ref(&input.published_upstream_ref);
    let push_result = branch_publish::push_session_branch_to_remote(
        &input.db,
        input.folder.clone(),
        Arc::clone(&input.git_client),
        PublishBranchAction::Push,
        &input.session_id,
        Some(remote_branch_name.as_str()),
        Some(&input.published_upstream_ref),
    )
    .await;

    match push_result {
        Ok(_) => {
            if let Some(metadata_sync_input) = input.review_request_metadata_sync.as_ref() {
                sync_linked_review_request_metadata_after_push(&input, metadata_sync_input).await;
            }
            resolve_review_comments_after_push(&input).await;

            let message = TranscriptNotice::BranchPush
                .format("Auto-pushed published branch after completed turn.");

            let _ = input
                .app_event_tx
                .send(AppEvent::PublishedBranchSyncUpdated {
                    persistent_notice: Some(message),
                    session_id: input.session_id,
                    sync_operation_id: input.sync_operation_id,
                    sync_status: PublishedBranchSyncStatus::Succeeded,
                });
        }
        Err(failure) => {
            let message = TranscriptNotice::BranchPushError.format(failure.message);

            let _ = input
                .app_event_tx
                .send(AppEvent::PublishedBranchSyncUpdated {
                    persistent_notice: Some(message),
                    session_id: input.session_id,
                    sync_operation_id: input.sync_operation_id,
                    sync_status: PublishedBranchSyncStatus::Failed,
                });
        }
    }
}

/// Posts agent-authored replies and resolves fixed allowlisted review threads
/// after the updated branch is visible on the forge.
async fn resolve_review_comments_after_push(input: &PublishedBranchAutoPushInput) {
    let operations = match input
        .db
        .reviews()
        .load_session_review_comment_resolutions(&input.session_id)
        .await
    {
        Ok(operations) if operations.is_empty() => return,
        Ok(operations) => operations,
        Err(error) => {
            append_review_comment_operation_load_failure_notice(input, &error.to_string()).await;

            return;
        }
    };
    let Some(operations) = review_comment_operations_matching_pushed_head(input, operations).await
    else {
        return;
    };
    if operations.is_empty() {
        return;
    }
    let expected_reply_count = operations.len();
    let expected_resolution_count = operations
        .iter()
        .filter(|operation| operation.resolution == "fixed")
        .count();
    let linked_review_request = match load_open_review_request(input).await {
        Ok(Some(linked_review_request)) => linked_review_request,
        Ok(None) => {
            append_missing_open_review_request_notice(input, expected_reply_count).await;

            return;
        }
        Err(error) => {
            append_review_comment_resolution_notice(
                input,
                0,
                expected_reply_count,
                0,
                expected_resolution_count,
            )
            .await;
            tracing::warn!(
                session_id = %input.session_id,
                %error,
                "failed to load linked review request for review-thread resolution"
            );

            return;
        }
    };
    let operations = operations
        .into_iter()
        .filter(|operation| {
            operation.review_request_display_id == linked_review_request.summary.display_id
        })
        .collect::<Vec<_>>();
    if operations.is_empty() {
        append_missing_open_review_request_notice(input, expected_reply_count).await;

        return;
    }
    let expected_reply_count = operations.len();
    let expected_resolution_count = operations
        .iter()
        .filter(|operation| operation.resolution == "fixed")
        .count();
    let Some(remote) =
        review_comment_resolution_remote(input, expected_reply_count, expected_resolution_count)
            .await
    else {
        return;
    };
    let display_id = &linked_review_request.summary.display_id;
    let Some(live_snapshot) = live_review_comment_snapshot(
        input,
        &remote,
        display_id,
        expected_reply_count,
        expected_resolution_count,
    )
    .await
    else {
        return;
    };
    let (replied_count, resolved_count) =
        post_review_comment_operations(input, &operations, &remote, display_id, &live_snapshot)
            .await;
    append_review_comment_resolution_notice(
        input,
        replied_count,
        expected_reply_count,
        resolved_count,
        expected_resolution_count,
    )
    .await;
}

/// Keeps only operations whose fix commit exactly matches the pushed tip.
async fn review_comment_operations_matching_pushed_head(
    input: &PublishedBranchAutoPushInput,
    operations: Vec<SessionReviewCommentResolutionRow>,
) -> Option<Vec<SessionReviewCommentResolutionRow>> {
    let mut matching_operations = Vec::new();
    let mut stale_operations = Vec::new();
    let mut unbound_count = 0;

    for operation in operations {
        let Some(commit_hash) = operation.commit_hash.as_ref() else {
            unbound_count += 1;

            continue;
        };
        let reachability = input
            .git_client
            .get_ref_ahead_behind(
                input.folder.clone(),
                "HEAD".to_string(),
                commit_hash.clone(),
            )
            .await;
        match reachability {
            Ok((0, 0)) => matching_operations.push(operation),
            Ok(_) => stale_operations.push(operation),
            Err(error) => {
                append_review_comment_commit_verification_failure_notice(input, &error.to_string())
                    .await;

                return None;
            }
        }
    }

    let stale_count = stale_operations.len();
    for operation in &stale_operations {
        remove_review_comment_operation(input, operation).await;
    }
    if stale_count != 0 {
        append_stale_review_comment_operations_notice(input, stale_count).await;
    }
    if unbound_count != 0 {
        append_unbound_review_comment_operations_notice(input, unbound_count).await;
    }

    Some(matching_operations)
}

/// Resolves the authenticated forge remote used for post-push thread effects.
async fn review_comment_resolution_remote(
    input: &PublishedBranchAutoPushInput,
    expected_reply_count: usize,
    expected_resolution_count: usize,
) -> Option<forge::ForgeRemote> {
    let repo_url = match input.git_client.repo_url(input.folder.clone()).await {
        Ok(repo_url) => repo_url,
        Err(error) => {
            append_review_comment_resolution_notice(
                input,
                0,
                expected_reply_count,
                0,
                expected_resolution_count,
            )
            .await;
            tracing::warn!(
                session_id = %input.session_id,
                %error,
                "failed to resolve repository remote for review-thread resolution"
            );

            return None;
        }
    };
    match input
        .review_request_client
        .detect_remote(repo_url)
        .map(|remote| remote.with_command_working_directory(input.folder.clone()))
    {
        Ok(remote) => Some(remote),
        Err(error) => {
            append_review_comment_resolution_notice(
                input,
                0,
                expected_reply_count,
                0,
                expected_resolution_count,
            )
            .await;
            let error_detail = error.detail_message();
            tracing::warn!(
                session_id = %input.session_id,
                error = %error_detail,
                "failed to detect forge remote for review-thread resolution"
            );

            None
        }
    }
}

/// Refreshes live thread state so retries can skip already-posted replies and
/// avoid mutating threads that disappeared after selection.
async fn live_review_comment_snapshot(
    input: &PublishedBranchAutoPushInput,
    remote: &forge::ForgeRemote,
    display_id: &str,
    expected_reply_count: usize,
    expected_resolution_count: usize,
) -> Option<forge::ReviewCommentSnapshot> {
    match input
        .review_request_client
        .fetch_review_comment_snapshot(remote.clone(), display_id.to_string())
        .await
    {
        Ok(live_snapshot) => Some(live_snapshot),
        Err(error) => {
            append_review_comment_resolution_notice(
                input,
                0,
                expected_reply_count,
                0,
                expected_resolution_count,
            )
            .await;
            let error_detail = error.detail_message();
            tracing::warn!(
                session_id = %input.session_id,
                error = %error_detail,
                "failed to refresh review threads before applying outcomes"
            );

            None
        }
    }
}

/// Posts each accepted reply and resolves only fixed outcomes.
async fn post_review_comment_operations(
    input: &PublishedBranchAutoPushInput,
    operations: &[SessionReviewCommentResolutionRow],
    remote: &forge::ForgeRemote,
    display_id: &str,
    live_snapshot: &forge::ReviewCommentSnapshot,
) -> (usize, usize) {
    let mut replied_count = 0;
    let mut resolved_count = 0;

    for operation in operations {
        let (operation_replied_count, operation_resolved_count) =
            apply_review_comment_operation(input, operation, remote, display_id, live_snapshot)
                .await;
        replied_count += operation_replied_count;
        resolved_count += operation_resolved_count;
    }

    (replied_count, resolved_count)
}

/// Applies one persisted reply and optional fixed-thread resolution.
async fn apply_review_comment_operation(
    input: &PublishedBranchAutoPushInput,
    operation: &SessionReviewCommentResolutionRow,
    remote: &forge::ForgeRemote,
    display_id: &str,
    live_snapshot: &forge::ReviewCommentSnapshot,
) -> (usize, usize) {
    let Some(thread_index) = live_snapshot
        .threads
        .iter()
        .position(|thread| thread.id == operation.thread_id)
    else {
        tracing::warn!(
            session_id = %input.session_id,
            thread_id = %operation.thread_id,
            "allowlisted review thread disappeared before outcome application"
        );

        return (0, 0);
    };
    let is_fixed = operation.resolution == "fixed";
    match ensure_review_comment_reply(
        input,
        operation,
        remote,
        display_id,
        live_snapshot,
        thread_index,
    )
    .await
    {
        ReviewCommentReplyProgress::Recorded => {}
        ReviewCommentReplyProgress::ThreadResolved => {
            remove_review_comment_operation(input, operation).await;

            return (0, usize::from(is_fixed));
        }
        ReviewCommentReplyProgress::Unavailable => return (0, 0),
    }
    let resolved_count = complete_review_comment_operation(
        input,
        operation,
        remote,
        display_id,
        live_snapshot.threads[thread_index].is_resolved,
        is_fixed,
    )
    .await;

    (1, resolved_count)
}

/// Ensures one reply is posted or recoverably recognized from a prior try.
async fn ensure_review_comment_reply(
    input: &PublishedBranchAutoPushInput,
    operation: &SessionReviewCommentResolutionRow,
    remote: &forge::ForgeRemote,
    display_id: &str,
    live_snapshot: &forge::ReviewCommentSnapshot,
    thread_index: usize,
) -> ReviewCommentReplyProgress {
    let reply_body = review_comment_reply_body(&operation.reply, &operation.reply_token);
    if operation.is_posting
        && live_snapshot.threads[thread_index]
            .comments
            .iter()
            .any(|comment| comment.body == reply_body)
    {
        return ReviewCommentReplyProgress::Recorded;
    }
    if live_snapshot.threads[thread_index].is_resolved {
        tracing::warn!(
            session_id = %input.session_id,
            thread_id = %operation.thread_id,
            "review thread was resolved before its agent reply could be posted"
        );

        return ReviewCommentReplyProgress::ThreadResolved;
    }
    if !operation.is_posting && !mark_review_comment_operation_posting(input, operation).await {
        return ReviewCommentReplyProgress::Unavailable;
    }
    let reply_result = input
        .review_request_client
        .reply_to_thread(
            remote.clone(),
            display_id.to_string(),
            operation.thread_id.clone(),
            reply_body,
        )
        .await;
    if let Err(error) = reply_result {
        let error_detail = error.detail_message();
        tracing::warn!(
            session_id = %input.session_id,
            thread_id = %operation.thread_id,
            error = %error_detail,
            "failed to reply to review thread"
        );

        return ReviewCommentReplyProgress::Unavailable;
    }
    ReviewCommentReplyProgress::Recorded
}

/// Completes a replied operation, resolving the forge thread when requested.
async fn complete_review_comment_operation(
    input: &PublishedBranchAutoPushInput,
    operation: &SessionReviewCommentResolutionRow,
    remote: &forge::ForgeRemote,
    display_id: &str,
    is_thread_resolved: bool,
    is_fixed: bool,
) -> usize {
    if !is_fixed {
        remove_review_comment_operation(input, operation).await;

        return 0;
    }
    if is_thread_resolved {
        remove_review_comment_operation(input, operation).await;

        return 1;
    }
    let resolve_result = input
        .review_request_client
        .resolve_thread(
            remote.clone(),
            display_id.to_string(),
            operation.thread_id.clone(),
        )
        .await;
    match resolve_result {
        Ok(()) => {
            remove_review_comment_operation(input, operation).await;

            1
        }
        Err(error) => {
            let error_detail = error.detail_message();
            tracing::warn!(
                session_id = %input.session_id,
                thread_id = %operation.thread_id,
                error = %error_detail,
                "failed to resolve replied review thread"
            );

            0
        }
    }
}

/// Records that one operation may expose its reply token to the forge.
async fn mark_review_comment_operation_posting(
    input: &PublishedBranchAutoPushInput,
    operation: &SessionReviewCommentResolutionRow,
) -> bool {
    match input
        .db
        .reviews()
        .mark_session_review_comment_resolution_posting(&input.session_id, &operation.reply_token)
        .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                session_id = %input.session_id,
                thread_id = %operation.thread_id,
                %error,
                "failed to persist review-comment operation progress"
            );

            false
        }
    }
}

/// Removes one operation after its requested forge effects finish.
async fn remove_review_comment_operation(
    input: &PublishedBranchAutoPushInput,
    operation: &SessionReviewCommentResolutionRow,
) {
    if let Err(error) = input
        .db
        .reviews()
        .remove_session_review_comment_resolution(&input.session_id, &operation.reply_token)
        .await
    {
        tracing::warn!(
            session_id = %input.session_id,
            thread_id = %operation.thread_id,
            %error,
            "failed to remove completed review-comment operation"
        );
    }
}

/// Appends an operation-specific non-rendering identity marker to one reply.
fn review_comment_reply_body(reply: &str, reply_token: &str) -> String {
    format!("{reply}\n\n{REVIEW_COMMENT_REPLY_MARKER_PREFIX}{reply_token} -->")
}

/// Reports that durable review-comment work could not be loaded after push.
async fn append_review_comment_operation_load_failure_notice(
    input: &PublishedBranchAutoPushInput,
    error: &str,
) {
    let message = TranscriptNotice::ReviewCommentsWarning.format(format!(
        "Could not load saved review-comment operations after the branch push: {error}"
    ));
    SessionTaskService::append_workflow_notice(
        &input.transcript,
        &input.db,
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
        &message,
    )
    .await;
}

/// Reports that commit ancestry could not be checked after a successful push.
async fn append_review_comment_commit_verification_failure_notice(
    input: &PublishedBranchAutoPushInput,
    error: &str,
) {
    let message = TranscriptNotice::ReviewCommentsWarning.format(format!(
        "Could not verify saved review-comment commits after the branch push: {error}. The saved \
         operations will retry after the next successful push."
    ));
    SessionTaskService::append_workflow_notice(
        &input.transcript,
        &input.db,
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
        &message,
    )
    .await;
}

/// Reports saved outcomes discarded after the pushed tip changed.
async fn append_stale_review_comment_operations_notice(
    input: &PublishedBranchAutoPushInput,
    stale_count: usize,
) {
    let message = TranscriptNotice::ReviewCommentsWarning.format(format!(
        "Discarded {stale_count} saved review thread update(s) because the pushed branch tip no \
         longer exactly matches the reported fix commit. Reopen review comments to retry."
    ));
    SessionTaskService::append_workflow_notice(
        &input.transcript,
        &input.db,
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
        &message,
    )
    .await;
}

/// Reports operations retained after commit binding was interrupted.
async fn append_unbound_review_comment_operations_notice(
    input: &PublishedBranchAutoPushInput,
    unbound_count: usize,
) {
    let message = TranscriptNotice::ReviewCommentsWarning.format(format!(
        "Kept {unbound_count} saved review thread update(s) pending because Agentty could not \
         finish binding them to the committed revision. Reopen those comments and run a fresh \
         agent turn to retry."
    ));
    SessionTaskService::append_workflow_notice(
        &input.transcript,
        &input.db,
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
        &message,
    )
    .await;
}

/// Reports that the review request disappeared or became terminal between
/// comment selection and post-push outcome application.
async fn append_missing_open_review_request_notice(
    input: &PublishedBranchAutoPushInput,
    expected_reply_count: usize,
) {
    let message = TranscriptNotice::ReviewCommentsWarning.format(format!(
        "Skipped {expected_reply_count} review thread update(s) because the session no longer has \
         an open linked review request. The saved operation will retry after the link is restored \
         and the branch is pushed again."
    ));
    SessionTaskService::append_workflow_notice(
        &input.transcript,
        &input.db,
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
        &message,
    )
    .await;
}

/// Appends a concise durable result for post-push review-thread updates.
async fn append_review_comment_resolution_notice(
    input: &PublishedBranchAutoPushInput,
    replied_count: usize,
    expected_reply_count: usize,
    resolved_count: usize,
    expected_resolution_count: usize,
) {
    let message =
        if replied_count == expected_reply_count && resolved_count == expected_resolution_count {
            TranscriptNotice::ReviewComments.format(format!(
                "Replied to {replied_count} review thread(s) and resolved {resolved_count} fixed \
                 thread(s)."
            ))
        } else {
            TranscriptNotice::ReviewCommentsWarning.format(format!(
                "Replied to {replied_count} of {expected_reply_count} review thread(s) and \
                 resolved {resolved_count} of {expected_resolution_count} fixed thread(s). The \
                 saved operation will retry after the next successful branch push."
            ))
        };
    SessionTaskService::append_workflow_notice(
        &input.transcript,
        &input.db,
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
        &message,
    )
    .await;
}

/// Syncs linked open review-request metadata after the new commit has reached
/// the already-published remote branch.
async fn sync_linked_review_request_metadata_after_push(
    input: &PublishedBranchAutoPushInput,
    metadata_sync_input: &ReviewRequestMetadataSyncInput,
) {
    let linked_review_request = match load_open_review_request(input).await {
        Ok(Some(linked_review_request)) => linked_review_request,
        Ok(None) => return,
        Err(error) => {
            append_review_request_sync_warning(input, error).await;

            return;
        }
    };
    let commit_message = match metadata_sync_input.commit_message.as_deref() {
        Some(commit_message) => commit_message.to_string(),
        None => match input
            .git_client
            .head_commit_message(input.folder.clone())
            .await
        {
            Ok(Some(commit_message)) => commit_message,
            Ok(None) => return,
            Err(error) => {
                append_review_request_sync_warning(
                    input,
                    SessionError::Workflow(format!(
                        "Failed to resolve the session commit message: {error}"
                    )),
                )
                .await;

                return;
            }
        },
    };
    let Some(generated_metadata) =
        crate::app::review_request::parse_review_request_commit_message(&commit_message)
    else {
        return;
    };

    let result = sync_review_request_metadata(
        input,
        metadata_sync_input,
        &linked_review_request,
        &metadata_sync_input.evaluation,
        generated_metadata,
    )
    .await;
    if let Err(error) = result {
        append_review_request_sync_warning(input, error).await;
    }
}

/// Loads the linked review request when it is still open.
async fn load_open_review_request(
    input: &PublishedBranchAutoPushInput,
) -> Result<Option<ReviewRequest>, SessionError> {
    let review_request = input
        .db
        .reviews()
        .load_session_review_request(&input.session_id)
        .await
        .map_err(SessionError::from)?
        .and_then(review_request_from_row);

    Ok(review_request
        .filter(|review_request| review_request.summary.state == ReviewRequestState::Open))
}

/// Converts one persisted review-request row into the domain model used by
/// session workflows.
fn review_request_from_row(
    row: crate::infra::db::SessionReviewRequestRow,
) -> Option<ReviewRequest> {
    Some(ReviewRequest {
        last_refreshed_at: row.last_refreshed_at,
        summary: forge::ReviewRequestSummary {
            display_id: row.display_id,
            forge_kind: forge::ForgeKind::from_str(&row.forge_kind).ok()?,
            source_branch: row.source_branch,
            state: ReviewRequestState::from_str(&row.state).ok()?,
            status_summary: row.status_summary,
            target_branch: row.target_branch,
            title: row.title,
            web_url: row.web_url,
        },
    })
}

/// Runs the forge metadata sync and persists the refreshed review-request
/// summary when the provider call succeeds.
async fn sync_review_request_metadata(
    input: &PublishedBranchAutoPushInput,
    metadata_sync_input: &ReviewRequestMetadataSyncInput,
    linked_review_request: &ReviewRequest,
    evaluation: &ReviewRequestMetadataEvaluationInput,
    generated_metadata: crate::app::review_request::ReviewRequestCommitMessage,
) -> Result<(), SessionError> {
    let repo_url = input
        .git_client
        .repo_url(input.folder.clone())
        .await
        .map_err(|error| {
            SessionError::Workflow(format!(
                "Failed to resolve repository remote for review-request metadata sync: {error}"
            ))
        })?;
    let remote = metadata_sync_input
        .review_request_client
        .detect_remote(repo_url)
        .map(|remote| remote.with_command_working_directory(input.folder.clone()))
        .map_err(|error| SessionError::Workflow(error.detail_message()))?;
    let current_metadata = metadata_sync_input
        .review_request_client
        .review_request_metadata(
            remote.clone(),
            linked_review_request.summary.display_id.clone(),
        )
        .await
        .map_err(|error| SessionError::Workflow(error.detail_message()))?;
    let desired_metadata = SessionTaskService::review_request_metadata(
        &current_metadata,
        &input.folder,
        generated_metadata.body.as_deref().unwrap_or_default(),
        &generated_metadata.title,
        evaluation.one_shot_client.as_ref(),
        evaluation.session_agent,
    )
    .await?;
    let update_input = forge::UpdateReviewRequestInput {
        body: Some(forge::ReviewRequestMetadataFieldUpdate {
            current: current_metadata.body,
            desired: desired_metadata.body,
        }),
        title: Some(forge::ReviewRequestMetadataFieldUpdate {
            current: current_metadata.title,
            desired: desired_metadata.title,
        }),
    };
    let summary = metadata_sync_input
        .review_request_client
        .sync_review_request_metadata(
            remote,
            linked_review_request.summary.display_id.clone(),
            update_input,
        )
        .await
        .map_err(|error| SessionError::Workflow(error.detail_message()))?;
    let review_request = ReviewRequest {
        last_refreshed_at: unix_timestamp_from_system_time(
            metadata_sync_input.clock.now_system_time(),
        ),
        summary,
    };

    input
        .db
        .reviews()
        .update_session_review_request(&input.session_id, Some(review_request))
        .await?;
    SessionTaskService::emit_session_updated(
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
    );
    let _ = input.app_event_tx.send(AppEvent::RefreshSessions);

    Ok(())
}

/// Appends one metadata-sync warning to the session transcript.
async fn append_review_request_sync_warning(
    input: &PublishedBranchAutoPushInput,
    error: SessionError,
) {
    warn_review_request_metadata_sync(input, &error.to_string());
    let message = TranscriptNotice::ReviewRequestSyncWarning.format(format!(
        "Failed to update linked review-request metadata: {error}"
    ));
    SessionTaskService::append_workflow_notice(
        &input.transcript,
        &input.db,
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
        &message,
    )
    .await;
}

/// Logs a best-effort review-request metadata sync warning.
fn warn_review_request_metadata_sync(input: &PublishedBranchAutoPushInput, error: &str) {
    tracing::warn!(
        session_id = %input.session_id,
        error,
        "failed to sync linked review-request metadata"
    );
}

#[cfg(test)]
mod tests {
    use ag_forge::{MockReviewRequestClient, ReviewCommentAnchorSide, ReviewCommentThread};
    use ag_git::{GitError, MockGitClient};
    use ag_protocol::{ReviewCommentOutcome, ReviewCommentResolution};

    use super::*;

    #[derive(Clone, Copy)]
    enum TestCommitComparison {
        Error,
        Matching,
        Rewritten,
        Unbound,
    }

    #[tokio::test]
    async fn metadata_sync_reconciles_live_remote_metadata_without_persisted_baselines() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_review_request_metadata()
            .once()
            .withf(|_, display_id| display_id == "#42")
            .returning(|_, _| {
                Box::pin(async {
                    Ok(forge::ReviewRequestMetadata {
                        body: "Tracks #42: https://example.com/issues/42".to_string(),
                        title: "Manual stable title".to_string(),
                    })
                })
            });
        review_request_client
            .expect_sync_review_request_metadata()
            .once()
            .withf(|_, display_id, input| {
                display_id == "#42"
                    && input.title.as_ref().is_some_and(|title| {
                        title.current == "Manual stable title"
                            && title.desired == "Manual stable title"
                    })
                    && input.body.as_ref().is_some_and(|body| {
                        body.current == "Tracks #42: https://example.com/issues/42"
                            && body.desired
                                == "Tracks #42: https://example.com/issues/42\n\nNew body."
                    })
            })
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(forge::ReviewRequestSummary {
                        display_id: "#42".to_string(),
                        forge_kind: forge::ForgeKind::GitHub,
                        source_branch: "wt/session-id".to_string(),
                        state: ReviewRequestState::Open,
                        status_summary: None,
                        target_branch: "main".to_string(),
                        title: "Manual stable title".to_string(),
                        web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                    })
                })
            });
        let mut one_shot_client = ag_agent::MockOneShotClient::new();
        one_shot_client.expect_submit().once().returning(|request| {
            assert!(request.prompt.contains("https://example.com/issues/42"));

            Ok(ag_agent::OneShotSubmission {
                response: ag_protocol::AgentResponse::plain(
                    r#"{"title":"Manual stable title","description":"Tracks #42: https://example.com/issues/42\n\nNew body.","is_title_change_significant":false}"#,
                ),
                stats: ag_agent::SessionStats::default(),
            })
        });
        let (input, transcript) = metadata_sync_test_input(db.clone(), git_client);
        let metadata_sync_input = ReviewRequestMetadataSyncInput {
            clock: Arc::new(crate::infra::clock::RealClock),
            commit_message: Some("Generated title\n\nNew body.".to_string()),
            evaluation: ReviewRequestMetadataEvaluationInput {
                one_shot_client: Arc::new(one_shot_client),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Codex,
                    crate::domain::agent::AgentModel::Gpt56Sol,
                ),
            },
            review_request_client: Arc::new(review_request_client),
        };

        // Act
        sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;
        let review_request = db
            .reviews()
            .load_session_review_request("session-id")
            .await
            .expect("failed to load linked review request")
            .expect("review request should remain linked");

        // Assert
        assert_eq!(review_request.title, "Manual stable title");
        assert!(transcript.lock().expect("transcript lock").is_empty());
    }

    #[tokio::test]
    async fn metadata_sync_reports_link_load_failure() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        insert_session(&db).await;
        link_open_review_request(&db, "#42").await;
        let (input, transcript) = metadata_sync_test_input(db, MockGitClient::new());
        let metadata_sync_input = metadata_sync_input(
            Some("Generated title\n\nNew body."),
            MockReviewRequestClient::new(),
        );
        pool.close().await;

        // Act
        sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;

        // Assert
        assert!(last_transcript_message(&transcript).contains(
            "Failed to update linked review-request metadata: attempted to acquire a connection \
             on a closed pool"
        ));
    }

    #[tokio::test]
    async fn metadata_sync_skips_missing_commit_message() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client
            .expect_head_commit_message()
            .once()
            .returning(|_| Box::pin(async { Ok(None) }));
        let (input, transcript) = metadata_sync_test_input(db, git_client);
        let metadata_sync_input = metadata_sync_input(None, MockReviewRequestClient::new());

        // Act
        sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;

        // Assert
        assert!(transcript.lock().expect("transcript lock").is_empty());
    }

    #[tokio::test]
    async fn metadata_sync_skips_unstructured_commit_message() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_head_commit_message().never();
        let (input, transcript) = metadata_sync_test_input(db, git_client);
        let metadata_sync_input =
            metadata_sync_input(Some("\n \n"), MockReviewRequestClient::new());

        // Act
        sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;

        // Assert
        assert!(transcript.lock().expect("transcript lock").is_empty());
    }

    #[tokio::test]
    async fn metadata_sync_reports_repository_remote_failure() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Err(GitError::OutputParse("missing remote".to_string())) })
        });
        let (input, transcript) = metadata_sync_test_input(db, git_client);
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client.expect_detect_remote().never();
        let metadata_sync_input =
            metadata_sync_input(Some("Generated title\n\nNew body."), review_request_client);

        // Act
        sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;

        // Assert
        let message = last_transcript_message(&transcript);
        assert!(
            message
                .contains("Failed to resolve repository remote for review-request metadata sync")
        );
        assert!(message.contains("missing remote"));
    }

    #[tokio::test]
    async fn review_comment_resolution_reports_reply_and_resolution_failures() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .returning(|_, _| {
                Box::pin(async { Ok(review_comment_snapshot(&["reply-fails", "resolve-fails"])) })
            });
        review_request_client
            .expect_reply_to_thread()
            .withf(|_, _, thread_id, _| thread_id == "reply-fails")
            .once()
            .returning(|_, _, _, _| {
                Box::pin(async {
                    Err(forge::ReviewRequestError::OperationFailed {
                        forge_kind: forge::ForgeKind::GitHub,
                        message: "reply rejected".to_string(),
                    })
                })
            });
        review_request_client
            .expect_reply_to_thread()
            .withf(|_, _, thread_id, _| thread_id == "resolve-fails")
            .once()
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        review_request_client
            .expect_resolve_thread()
            .withf(|_, _, thread_id| thread_id == "resolve-fails")
            .once()
            .returning(|_, _, _| {
                Box::pin(async {
                    Err(forge::ReviewRequestError::OperationFailed {
                        forge_kind: forge::ForgeKind::GitHub,
                        message: "resolve rejected".to_string(),
                    })
                })
            });
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![fixed_outcome("reply-fails"), fixed_outcome("resolve-fails")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 1 of 2 review thread(s) and resolved 0 of 2 \
             fixed thread(s). The saved operation will retry after the next successful branch \
             push."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_replies_to_all_outcomes_and_resolves_only_fixed() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .returning(|_, _| {
                Box::pin(async { Ok(review_comment_snapshot(&["no-change", "fixed"])) })
            });
        review_request_client
            .expect_reply_to_thread()
            .withf(|_, _, thread_id, body| {
                (thread_id == "no-change"
                    && body
                        == "The current implementation is already safe.\n\n<!-- agentty review \
                            resolution:token-no-change -->")
                    || (thread_id == "fixed"
                        && body
                            == "Addressed fixed.\n\n<!-- agentty review resolution:token-fixed -->")
            })
            .times(2)
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        review_request_client
            .expect_resolve_thread()
            .withf(|_, _, thread_id| thread_id == "fixed")
            .once()
            .returning(|_, _, _| Box::pin(async { Ok(()) }));
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![no_change_outcome("no-change"), fixed_outcome("fixed")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments] Replied to 2 review thread(s) and resolved 1 fixed thread(s)."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_reuses_matching_reply_before_retrying_resolution() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut live_snapshot = review_comment_snapshot(&["fixed"]);
        live_snapshot.threads[0]
            .comments
            .push(forge::ReviewComment {
                author: "agentty".to_string(),
                body: review_comment_reply_body("Addressed fixed.", "token-fixed"),
            });
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .return_once(|_, _| Box::pin(async move { Ok(live_snapshot) }));
        review_request_client.expect_reply_to_thread().never();
        review_request_client
            .expect_resolve_thread()
            .withf(|_, _, thread_id| thread_id == "fixed")
            .once()
            .returning(|_, _, _| Box::pin(async { Ok(()) }));
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![fixed_outcome("fixed")],
        )
        .await;
        input
            .db
            .reviews()
            .mark_session_review_comment_resolution_posting("session-id", "token-fixed")
            .await
            .expect("failed to persist posting state");

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments] Replied to 1 review thread(s) and resolved 1 fixed thread(s)."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_does_not_trust_matching_reply_while_pending() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut live_snapshot = review_comment_snapshot(&["fixed"]);
        live_snapshot.threads[0]
            .comments
            .push(forge::ReviewComment {
                author: "collaborator".to_string(),
                body: review_comment_reply_body("Addressed fixed.", "token-fixed"),
            });
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .return_once(|_, _| Box::pin(async move { Ok(live_snapshot) }));
        review_request_client
            .expect_reply_to_thread()
            .once()
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        review_request_client
            .expect_resolve_thread()
            .once()
            .returning(|_, _, _| Box::pin(async { Ok(()) }));
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![fixed_outcome("fixed")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments] Replied to 1 review thread(s) and resolved 1 fixed thread(s)."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_reports_disappeared_live_thread() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .returning(|_, _| Box::pin(async { Ok(forge::ReviewCommentSnapshot::default()) }));
        review_request_client.expect_reply_to_thread().never();
        review_request_client.expect_resolve_thread().never();
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![fixed_outcome("missing")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 \
             fixed thread(s). The saved operation will retry after the next successful branch \
             push."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_reports_live_snapshot_failure() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .returning(|_, _| {
                Box::pin(async {
                    Err(forge::ReviewRequestError::OperationFailed {
                        forge_kind: forge::ForgeKind::GitHub,
                        message: "snapshot unavailable".to_string(),
                    })
                })
            });
        review_request_client.expect_reply_to_thread().never();
        review_request_client.expect_resolve_thread().never();
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![fixed_outcome("fixed")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 \
             fixed thread(s). The saved operation will retry after the next successful branch \
             push."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_does_not_reply_to_concurrently_resolved_thread() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut live_snapshot = review_comment_snapshot(&["fixed"]);
        live_snapshot.threads[0].is_resolved = true;
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .return_once(|_, _| Box::pin(async move { Ok(live_snapshot) }));
        review_request_client.expect_reply_to_thread().never();
        review_request_client.expect_resolve_thread().never();
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![fixed_outcome("fixed")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 1 of 1 \
             fixed thread(s). The saved operation will retry after the next successful branch \
             push."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_reports_repository_remote_failure() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async {
                Err(GitError::CommandFailed {
                    command: "git remote get-url origin".to_string(),
                    stderr: "missing remote".to_string(),
                })
            })
        });
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            MockReviewRequestClient::new(),
            vec![fixed_outcome("thread-1")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 \
             fixed thread(s). The saved operation will retry after the next successful branch \
             push."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_reports_remote_detection_failure() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client
            .expect_repo_url()
            .once()
            .returning(|_| Box::pin(async { Ok("ssh://example.com/owner/repo.git".to_string()) }));
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|repo_url| Err(forge::ReviewRequestError::UnsupportedRemote { repo_url }));
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![fixed_outcome("thread-1")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 \
             fixed thread(s). The saved operation will retry after the next successful branch \
             push."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_skips_sessions_without_open_linked_review() {
        // Arrange
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_session(&db).await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().never();
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            MockReviewRequestClient::new(),
            vec![fixed_outcome("thread-1")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Skipped 1 review thread update(s) because the session no \
             longer has an open linked review request. The saved operation will retry after the \
             link is restored and the branch is pushed again."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_reports_operation_load_failure() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        insert_session(&db).await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().never();
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            MockReviewRequestClient::new(),
            vec![fixed_outcome("thread-1")],
        )
        .await;
        pool.close().await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Could not load saved review-comment operations after the \
             branch push: attempted to acquire a connection on a closed pool"
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_reports_posting_progress_failure() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        insert_session(&db).await;
        link_open_review_request(&db, "#42").await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .return_once(move |_, _| {
                Box::pin(async move {
                    pool.close().await;

                    Ok(review_comment_snapshot(&["fixed"]))
                })
            });
        review_request_client.expect_reply_to_thread().never();
        review_request_client.expect_resolve_thread().never();
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![fixed_outcome("fixed")],
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 \
             fixed thread(s). The saved operation will retry after the next successful branch \
             push."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_counts_replied_thread_resolved_during_retry() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        insert_session(&db).await;
        link_open_review_request(&db, "#42").await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut live_snapshot = review_comment_snapshot(&["fixed"]);
        live_snapshot.threads[0].is_resolved = true;
        live_snapshot.threads[0]
            .comments
            .push(forge::ReviewComment {
                author: "agentty".to_string(),
                body: review_comment_reply_body("Addressed fixed.", "token-fixed"),
            });
        let mut review_request_client = MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .returning(|_| Ok(github_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .return_once(move |_, _| {
                Box::pin(async move {
                    pool.close().await;

                    Ok(live_snapshot)
                })
            });
        review_request_client.expect_reply_to_thread().never();
        review_request_client.expect_resolve_thread().never();
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            review_request_client,
            vec![fixed_outcome("fixed")],
        )
        .await;
        input
            .db
            .reviews()
            .mark_session_review_comment_resolution_posting("session-id", "token-fixed")
            .await
            .expect("failed to persist posting state");

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments] Replied to 1 review thread(s) and resolved 1 fixed thread(s)."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_reports_link_load_failure() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        insert_session(&db).await;
        db.reviews()
            .update_session_review_request("session-id", Some(open_review_request("#42")))
            .await
            .expect("failed to link review request");
        persist_resolution_test_operations(&db, vec![fixed_outcome("thread-1")], Some("commit-1"))
            .await;
        let mut git_client = MockGitClient::new();
        git_client
            .expect_get_ref_ahead_behind()
            .once()
            .return_once(move |_, _, _| {
                Box::pin(async move {
                    pool.close().await;

                    Ok((0, 0))
                })
            });
        git_client.expect_repo_url().never();
        let (input, transcript) =
            persisted_resolution_test_input(db, git_client, MockReviewRequestClient::new());

        // Act
        resolve_review_comments_after_push(&input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 \
             fixed thread(s). The saved operation will retry after the next successful branch \
             push."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_rejects_operations_for_an_old_review_request() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().never();
        let (input, transcript) = resolution_test_input(
            db.clone(),
            git_client,
            MockReviewRequestClient::new(),
            vec![fixed_outcome("thread-1")],
        )
        .await;
        db.reviews()
            .update_session_review_request("session-id", Some(open_review_request("#43")))
            .await
            .expect("failed to replace linked review request");

        // Act
        resolve_review_comments_after_push(&input).await;
        let operations = db
            .reviews()
            .load_session_review_comment_resolutions("session-id")
            .await
            .expect("failed to load retained review operation");

        // Assert
        assert_eq!(operations.len(), 1);
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Skipped 1 review thread update(s) because the session no \
             longer has an open linked review request. The saved operation will retry after the \
             link is restored and the branch is pushed again."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_discards_fix_commit_removed_by_rebase() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().never();
        let (input, transcript) = resolution_test_input_with_commit_reachability(
            db.clone(),
            git_client,
            MockReviewRequestClient::new(),
            vec![fixed_outcome("thread-1")],
            TestCommitComparison::Rewritten,
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;
        let operations = db
            .reviews()
            .load_session_review_comment_resolutions("session-id")
            .await
            .expect("failed to load discarded review operation");

        // Assert
        assert_eq!(operations, Vec::new());
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Discarded 1 saved review thread update(s) because the \
             pushed branch tip no longer exactly matches the reported fix commit. Reopen review \
             comments to retry."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_retains_unbound_operation_for_fresh_retry() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_get_ref_ahead_behind().never();
        git_client.expect_repo_url().never();
        let (input, transcript) = resolution_test_input_with_commit_reachability(
            db.clone(),
            git_client,
            MockReviewRequestClient::new(),
            vec![fixed_outcome("thread-1")],
            TestCommitComparison::Unbound,
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;
        let operations = db
            .reviews()
            .load_session_review_comment_resolutions("session-id")
            .await
            .expect("failed to load retained review operation");

        // Assert
        assert_eq!(operations.len(), 1);
        assert!(operations[0].commit_hash.is_none());
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Kept 1 saved review thread update(s) pending because \
             Agentty could not finish binding them to the committed revision. Reopen those \
             comments and run a fresh agent turn to retry."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_retries_when_commit_check_fails() {
        // Arrange
        let db = linked_review_request_db().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().never();
        let (input, transcript) = resolution_test_input_with_commit_reachability(
            db.clone(),
            git_client,
            MockReviewRequestClient::new(),
            vec![fixed_outcome("thread-1")],
            TestCommitComparison::Error,
        )
        .await;

        // Act
        resolve_review_comments_after_push(&input).await;
        let operations = db
            .reviews()
            .load_session_review_comment_resolutions("session-id")
            .await
            .expect("failed to load retained review operation");

        // Assert
        assert_eq!(operations.len(), 1);
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Could not verify saved review-comment commits after the \
             branch push: commit lookup failed. The saved operations will retry after the next \
             successful push."
        );
    }

    /// Builds one detached-push input for direct review-resolution tests.
    async fn resolution_test_input(
        db: AppRepositories,
        git_client: MockGitClient,
        review_request_client: MockReviewRequestClient,
        outcomes: Vec<ReviewCommentOutcome>,
    ) -> (PublishedBranchAutoPushInput, Arc<Mutex<SessionTranscript>>) {
        resolution_test_input_with_commit_reachability(
            db,
            git_client,
            review_request_client,
            outcomes,
            TestCommitComparison::Matching,
        )
        .await
    }

    /// Builds one resolution input with a deterministic commit comparison.
    async fn resolution_test_input_with_commit_reachability(
        db: AppRepositories,
        mut git_client: MockGitClient,
        review_request_client: MockReviewRequestClient,
        outcomes: Vec<ReviewCommentOutcome>,
        commit_comparison: TestCommitComparison,
    ) -> (PublishedBranchAutoPushInput, Arc<Mutex<SessionTranscript>>) {
        if !matches!(commit_comparison, TestCommitComparison::Unbound) {
            git_client
                .expect_get_ref_ahead_behind()
                .returning(move |_, left_ref, right_ref| {
                    assert_eq!(left_ref, "HEAD");
                    assert_eq!(right_ref, "commit-1");

                    Box::pin(async move {
                        match commit_comparison {
                            TestCommitComparison::Error => {
                                Err(GitError::OutputParse("commit lookup failed".to_string()))
                            }
                            TestCommitComparison::Matching => Ok((0, 0)),
                            TestCommitComparison::Rewritten => Ok((1, 1)),
                            TestCommitComparison::Unbound => unreachable!(),
                        }
                    })
                });
        }
        let commit_hash =
            (!matches!(commit_comparison, TestCommitComparison::Unbound)).then_some("commit-1");
        persist_resolution_test_operations(&db, outcomes, commit_hash).await;

        persisted_resolution_test_input(db, git_client, review_request_client)
    }

    /// Persists deterministic review operations for direct resolution tests.
    async fn persist_resolution_test_operations(
        db: &AppRepositories,
        outcomes: Vec<ReviewCommentOutcome>,
        commit_hash: Option<&str>,
    ) {
        let resolutions = outcomes
            .into_iter()
            .map(
                |outcome| crate::infra::db::NewSessionReviewCommentResolution {
                    commit_hash: commit_hash.map(str::to_string),
                    reply: outcome.reply,
                    reply_token: format!("token-{}", outcome.thread_id),
                    resolution: match outcome.resolution {
                        ReviewCommentResolution::Fixed => "fixed",
                        ReviewCommentResolution::NoChangeNeeded => "no_change_needed",
                    }
                    .to_string(),
                    review_request_display_id: "#42".to_string(),
                    thread_id: outcome.thread_id,
                },
            )
            .collect::<Vec<_>>();
        db.reviews()
            .insert_session_review_comment_resolutions("session-id", &resolutions)
            .await
            .expect("failed to persist review-comment resolutions");
    }

    /// Builds one resolution input after its durable operations are present.
    fn persisted_resolution_test_input(
        db: AppRepositories,
        git_client: MockGitClient,
        review_request_client: MockReviewRequestClient,
    ) -> (PublishedBranchAutoPushInput, Arc<Mutex<SessionTranscript>>) {
        let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
        let input = PublishedBranchAutoPushInput {
            app_event_tx: mpsc::unbounded_channel().0,
            db,
            folder: PathBuf::from("/tmp/project"),
            git_client: Arc::new(git_client),
            published_upstream_ref: "origin/wt/session-id".to_string(),
            review_request_client: Arc::new(review_request_client),
            review_request_metadata_sync: None,
            session_id: "session-id".into(),
            session_update_versions: Arc::default(),
            sync_operation_id: "sync-id".to_string(),
            transcript: Arc::clone(&transcript),
        };

        (input, transcript)
    }

    /// Builds one detached-push input for direct metadata-sync tests.
    fn metadata_sync_test_input(
        db: AppRepositories,
        git_client: MockGitClient,
    ) -> (PublishedBranchAutoPushInput, Arc<Mutex<SessionTranscript>>) {
        let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
        let input = PublishedBranchAutoPushInput {
            app_event_tx: mpsc::unbounded_channel().0,
            db,
            folder: PathBuf::from("/tmp/project"),
            git_client: Arc::new(git_client),
            published_upstream_ref: "origin/wt/session-id".to_string(),
            review_request_client: Arc::new(MockReviewRequestClient::new()),
            review_request_metadata_sync: None,
            session_id: "session-id".into(),
            session_update_versions: Arc::default(),
            sync_operation_id: "sync-id".to_string(),
            transcript: Arc::clone(&transcript),
        };

        (input, transcript)
    }

    /// Builds metadata-sync dependencies with deterministic provider mocks.
    fn metadata_sync_input(
        commit_message: Option<&str>,
        review_request_client: MockReviewRequestClient,
    ) -> ReviewRequestMetadataSyncInput {
        ReviewRequestMetadataSyncInput {
            clock: Arc::new(crate::infra::clock::RealClock),
            commit_message: commit_message.map(str::to_string),
            evaluation: ReviewRequestMetadataEvaluationInput {
                one_shot_client: Arc::new(ag_agent::MockOneShotClient::new()),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Codex,
                    crate::domain::agent::AgentModel::Gpt56Sol,
                ),
            },
            review_request_client: Arc::new(review_request_client),
        }
    }

    /// Builds one fixed outcome accepted by the post-turn allowlist.
    fn fixed_outcome(thread_id: &str) -> ReviewCommentOutcome {
        ReviewCommentOutcome {
            reply: format!("Addressed {thread_id}."),
            resolution: ag_protocol::ReviewCommentResolution::Fixed,
            thread_id: thread_id.to_string(),
        }
    }

    /// Builds one no-change outcome that receives a reply but remains open.
    fn no_change_outcome(thread_id: &str) -> ReviewCommentOutcome {
        ReviewCommentOutcome {
            reply: "The current implementation is already safe.".to_string(),
            resolution: ag_protocol::ReviewCommentResolution::NoChangeNeeded,
            thread_id: thread_id.to_string(),
        }
    }

    /// Builds one live unresolved forge snapshot for outcome-application
    /// tests.
    fn review_comment_snapshot(thread_ids: &[&str]) -> forge::ReviewCommentSnapshot {
        forge::ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads: thread_ids
                .iter()
                .map(|thread_id| ReviewCommentThread {
                    anchor_side: ReviewCommentAnchorSide::New,
                    comments: Vec::new(),
                    id: (*thread_id).to_string(),
                    is_outdated: Some(false),
                    is_resolved: false,
                    line: Some(1),
                    path: "src/lib.rs".to_string(),
                    start_line: None,
                })
                .collect(),
        }
    }

    /// Inserts one session linked to an open GitHub pull request.
    async fn linked_review_request_db() -> AppRepositories {
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_session(&db).await;
        link_open_review_request(&db, "#42").await;

        db
    }

    /// Links one open review request to the resolution-test session.
    async fn link_open_review_request(db: &AppRepositories, display_id: &str) {
        db.reviews()
            .update_session_review_request("session-id", Some(open_review_request(display_id)))
            .await
            .expect("failed to link review request");
    }

    /// Builds one open linked review request fixture.
    fn open_review_request(display_id: &str) -> ReviewRequest {
        ReviewRequest {
            last_refreshed_at: 100,
            summary: forge::ReviewRequestSummary {
                display_id: display_id.to_string(),
                forge_kind: forge::ForgeKind::GitHub,
                source_branch: "wt/session-id".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Review title".to_string(),
                web_url: format!(
                    "https://github.com/agentty-xyz/agentty/pull/{}",
                    display_id.trim_start_matches('#')
                ),
            },
        }
    }

    /// Inserts the session row required by review and transcript stores.
    async fn insert_session(db: &AppRepositories) {
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        db.sessions()
            .insert_session(
                "session-id",
                "gemini-3.7-flash",
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");
    }

    /// Returns one GitHub remote used by the forge mock.
    fn github_remote() -> forge::ForgeRemote {
        forge::ForgeRemote {
            command_working_directory: None,
            forge_kind: forge::ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        }
    }

    /// Reads the latest live transcript message.
    fn last_transcript_message(transcript: &Arc<Mutex<SessionTranscript>>) -> String {
        transcript
            .lock()
            .expect("transcript lock")
            .messages()
            .last()
            .expect("workflow notice")
            .content
            .trim()
            .to_string()
    }
}
