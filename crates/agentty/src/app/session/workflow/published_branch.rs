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
    format!(
        "{reply}\n\n{}{reply_token} -->",
        forge::AGENTTY_REVIEW_REPLY_MARKER_PREFIX
    )
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
#[path = "published_branch_test.rs"]
mod tests;
