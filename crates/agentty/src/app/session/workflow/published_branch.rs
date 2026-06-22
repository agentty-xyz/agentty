//! Published-branch post-turn synchronization for session workers.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use ag_forge as forge;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::SessionTaskService;
use super::worker::SessionWorkerContext;
use crate::app::session::{
    Clock, SessionError, remote_branch_name_from_upstream_ref, unix_timestamp_from_system_time,
};
use crate::app::{AppEvent, branch_publish};
use crate::domain::session::{
    PublishBranchAction, PublishedBranchSyncStatus, ReviewRequest, ReviewRequestState, SessionId,
};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::db::AppRepositories;
use crate::infra::git::GitClient;

/// Starts one detached auto-push task for a session that already tracks a
/// published upstream branch and has no queued follow-up messages waiting.
pub(super) fn start_published_branch_auto_push(
    context: &SessionWorkerContext,
    published_upstream_ref: Option<String>,
    review_request_commit_message: Option<String>,
) {
    if context.has_queued_messages() {
        return;
    }

    let Some(published_upstream_ref) = published_upstream_ref else {
        return;
    };

    let sync_operation_id = Uuid::new_v4().to_string();
    let session_id = context.session_id.clone();
    let app_event_tx = context.app_event_tx.clone();
    let db = context.db.clone();
    let folder = context.folder.clone();
    let git_client = Arc::clone(&context.git_client);
    let output = Arc::clone(&context.output);
    let review_request_metadata_sync =
        review_request_commit_message.map(|commit_message| ReviewRequestMetadataSyncInput {
            clock: Arc::clone(&context.clock),
            commit_message,
            review_request_client: Arc::clone(&context.review_request_client),
        });
    let session_update_versions = context.session_update_versions.clone();

    let _ = app_event_tx.send(AppEvent::PublishedBranchSyncUpdated {
        session_id: session_id.clone(),
        sync_operation_id: sync_operation_id.clone(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    });

    let auto_push_input = PublishedBranchAutoPushInput {
        app_event_tx,
        db,
        folder,
        git_client,
        output,
        published_upstream_ref,
        review_request_metadata_sync,
        session_id,
        session_update_versions,
        sync_operation_id,
    };
    tokio::spawn(async move {
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
    /// Shared transcript buffer used to append push failure messages.
    pub(super) output: Arc<Mutex<String>>,
    /// Published upstream reference that provides the remote branch target.
    pub(super) published_upstream_ref: String,
    /// Optional metadata sync payload used after a successful post-turn push.
    pub(super) review_request_metadata_sync: Option<ReviewRequestMetadataSyncInput>,
    /// Session id whose branch is being pushed.
    pub(super) session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: crate::app::service::SessionUpdateVersionMap,
    /// Auto-push operation id used to ignore stale completion updates.
    pub(super) sync_operation_id: String,
}

/// Owned dependencies for one optional linked PR/MR metadata sync after push.
pub(super) struct ReviewRequestMetadataSyncInput {
    /// Clock used to timestamp the refreshed review-request summary.
    pub(super) clock: Arc<dyn Clock>,
    /// Latest auto-commit message used to update linked PR/MR metadata.
    pub(super) commit_message: String,
    /// Forge boundary used to refresh linked PR/MR metadata after a push.
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
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

            let _ = input
                .app_event_tx
                .send(AppEvent::PublishedBranchSyncUpdated {
                    session_id: input.session_id,
                    sync_operation_id: input.sync_operation_id,
                    sync_status: PublishedBranchSyncStatus::Succeeded,
                });
        }
        Err(failure) => {
            let message = TranscriptNotice::BranchPushError.format(failure.message);
            SessionTaskService::append_session_output(
                &input.output,
                &input.db,
                &input.app_event_tx,
                &input.session_update_versions,
                &input.session_id,
                &message,
            )
            .await;

            let _ = input
                .app_event_tx
                .send(AppEvent::PublishedBranchSyncUpdated {
                    session_id: input.session_id,
                    sync_operation_id: input.sync_operation_id,
                    sync_status: PublishedBranchSyncStatus::Failed,
                });
        }
    }
}

/// Syncs linked open review-request metadata after the new commit has reached
/// the already-published remote branch.
async fn sync_linked_review_request_metadata_after_push(
    input: &PublishedBranchAutoPushInput,
    metadata_sync_input: &ReviewRequestMetadataSyncInput,
) {
    let Some(update_input) = review_request_update_input(&metadata_sync_input.commit_message)
    else {
        return;
    };
    let Some(linked_review_request) = load_open_review_request(input).await else {
        return;
    };

    let result = sync_review_request_metadata(
        input,
        metadata_sync_input,
        &linked_review_request,
        update_input,
    )
    .await;
    if let Err(error) = result {
        append_review_request_sync_warning(input, error).await;
    }
}

/// Builds an update payload from one canonical session commit message.
fn review_request_update_input(commit_message: &str) -> Option<forge::UpdateReviewRequestInput> {
    let review_request_commit_message =
        crate::app::review_request::parse_review_request_commit_message(commit_message)?;

    Some(forge::UpdateReviewRequestInput {
        body: review_request_commit_message.body,
        title: review_request_commit_message.title,
    })
}

/// Loads the linked review request when it is still open.
async fn load_open_review_request(input: &PublishedBranchAutoPushInput) -> Option<ReviewRequest> {
    let review_request = match input
        .db
        .reviews()
        .load_session_review_request(&input.session_id)
        .await
    {
        Ok(Some(row)) => review_request_from_row(row),
        Ok(None) => None,
        Err(error) => {
            warn_review_request_metadata_sync(
                input,
                &format!("failed to load linked review request: {error}"),
            );

            None
        }
    }?;

    (review_request.summary.state == ReviewRequestState::Open).then_some(review_request)
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
    update_input: forge::UpdateReviewRequestInput,
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
    SessionTaskService::append_session_output(
        &input.output,
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
