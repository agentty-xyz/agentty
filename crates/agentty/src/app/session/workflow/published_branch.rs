//! Published-branch post-turn synchronization for session workers.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use ag_forge as forge;
use ag_git::GitClient;
use tokio::sync::{OwnedMutexGuard, mpsc};
use uuid::Uuid;

use super::SessionTaskService;
use crate::app::session::{
    Clock, SessionError, remote_branch_name_from_upstream_ref, unix_timestamp_from_system_time,
};
use crate::app::{AppEvent, branch_publish};
use crate::domain::session::{PublishBranchAction, ReviewRequest, ReviewRequestState, SessionId};
use crate::domain::session_message::{SessionMessageKind, SessionMessageState, SessionTranscript};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::db::{AppRepositories, SessionTimelineMessage};

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
    /// Published upstream reference that provides the remote branch target.
    pub(super) published_upstream_ref: String,
    /// Forge boundary used for optional linked PR/MR metadata refresh.
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Optional auto-commit message used to refresh linked PR/MR metadata.
    pub(super) review_request_commit_message: Option<String>,
    /// Session id whose branch is being pushed.
    pub(super) session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: crate::app::service::SessionUpdateVersionMap,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

/// Starts one detached auto-push task for a session that already tracks a
/// published upstream branch.
pub(super) async fn start_published_branch_auto_push(input: PublishedBranchAutoPushStartInput) {
    let branch_operation_guard = input.branch_operation_guard;
    let sync_operation_id = Uuid::new_v4().to_string();
    let entry_key = format!("branch_push:{sync_operation_id}");
    let review_request_metadata_sync =
        input
            .review_request_commit_message
            .map(|commit_message| ReviewRequestMetadataSyncInput {
                clock: Arc::clone(&input.clock),
                commit_message: Some(commit_message),
                review_request_client: Arc::clone(&input.review_request_client),
            });

    let turn_id = input
        .transcript
        .lock()
        .map_or(0, |transcript| transcript.current_turn_id());
    if let Err(error) = SessionTaskService::upsert_timeline_message(
        &input.transcript,
        &input.db,
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
        SessionTimelineMessage {
            content: "Auto-pushing published branch after completed turn...",
            entry_key: &entry_key,
            kind: SessionMessageKind::WorkflowNotice,
            state: SessionMessageState::Pending,
            turn_id,
        },
    )
    .await
    {
        tracing::warn!(
            session_id = %input.session_id,
            %error,
            "failed to post published-branch timeline entry"
        );
    }

    let auto_push_input = PublishedBranchAutoPushInput {
        app_event_tx: input.app_event_tx,
        db: input.db,
        entry_key,
        folder: input.folder,
        git_client: input.git_client,
        published_upstream_ref: input.published_upstream_ref,
        review_request_metadata_sync,
        session_id: input.session_id,
        session_update_versions: input.session_update_versions,
        transcript: input.transcript,
        turn_id,
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
    /// Stable timeline identity replaced when the push completes.
    pub(super) entry_key: String,
    /// Session worktree folder pushed to its tracked upstream branch.
    pub(super) folder: PathBuf,
    /// Git boundary used for the remote push operation.
    pub(super) git_client: Arc<dyn GitClient>,
    /// Published upstream reference that provides the remote branch target.
    pub(super) published_upstream_ref: String,
    /// Optional metadata sync payload used after a successful post-turn push.
    pub(super) review_request_metadata_sync: Option<ReviewRequestMetadataSyncInput>,
    /// Session id whose branch is being pushed.
    pub(super) session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: crate::app::service::SessionUpdateVersionMap,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
    /// Turn that owned the push when its pending row was inserted.
    pub(super) turn_id: i64,
}

/// Owned dependencies for one optional linked PR/MR metadata sync after push.
pub(super) struct ReviewRequestMetadataSyncInput {
    /// Clock used to timestamp the refreshed review-request summary.
    pub(super) clock: Arc<dyn Clock>,
    /// Known auto-commit message, or `None` to resolve it after the push.
    pub(super) commit_message: Option<String>,
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

            let message = TranscriptNotice::BranchPush
                .format("Auto-pushed published branch after completed turn.");
            let _ = SessionTaskService::upsert_timeline_message(
                &input.transcript,
                &input.db,
                &input.app_event_tx,
                &input.session_update_versions,
                &input.session_id,
                SessionTimelineMessage {
                    content: &message,
                    entry_key: &input.entry_key,
                    kind: SessionMessageKind::WorkflowNotice,
                    state: SessionMessageState::Resolved,
                    turn_id: input.turn_id,
                },
            )
            .await;
            SessionTaskService::request_git_status_refresh(&input.app_event_tx);
        }
        Err(failure) => {
            let message = TranscriptNotice::BranchPushError.format(failure.message);
            let _ = SessionTaskService::upsert_timeline_message(
                &input.transcript,
                &input.db,
                &input.app_event_tx,
                &input.session_update_versions,
                &input.session_id,
                SessionTimelineMessage {
                    content: &message,
                    entry_key: &input.entry_key,
                    kind: SessionMessageKind::WorkflowNotice,
                    state: SessionMessageState::Failed,
                    turn_id: input.turn_id,
                },
            )
            .await;
        }
    }
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
    let Some(update_input) = review_request_update_input(&commit_message) else {
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

/// Persists one metadata-sync warning under the turn that started the push.
async fn append_review_request_sync_warning(
    input: &PublishedBranchAutoPushInput,
    error: SessionError,
) {
    warn_review_request_metadata_sync(input, &error.to_string());
    let message = TranscriptNotice::ReviewRequestSyncWarning.format(format!(
        "Failed to update linked review-request metadata: {error}"
    ));
    let entry_key = format!("{}:review_request_sync", input.entry_key);
    if let Err(persistence_error) = SessionTaskService::upsert_timeline_message(
        &input.transcript,
        &input.db,
        &input.app_event_tx,
        &input.session_update_versions,
        &input.session_id,
        SessionTimelineMessage {
            content: &message,
            entry_key: &entry_key,
            kind: SessionMessageKind::WorkflowNotice,
            state: SessionMessageState::Failed,
            turn_id: input.turn_id,
        },
    )
    .await
    {
        tracing::warn!(
            session_id = %input.session_id,
            error = %persistence_error,
            "failed to persist review-request metadata warning"
        );
    }
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
    use super::*;

    #[tokio::test]
    async fn test_review_request_sync_warning_keeps_captured_push_turn() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session("session-id", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        db.sessions()
            .append_session_message("session-id", SessionMessageKind::UserPrompt, "Push owner")
            .await
            .expect("failed to append push owner prompt");
        db.sessions()
            .append_session_message("session-id", SessionMessageKind::UserPrompt, "Later reply")
            .await
            .expect("failed to append later prompt");
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let input = PublishedBranchAutoPushInput {
            app_event_tx,
            db: db.clone(),
            entry_key: "branch_push:operation-id".to_string(),
            folder: PathBuf::from("/tmp/session-id"),
            git_client: Arc::new(ag_git::MockGitClient::new()),
            published_upstream_ref: "origin/wt/session-id".to_string(),
            review_request_metadata_sync: None,
            session_id: "session-id".into(),
            session_update_versions: Arc::default(),
            transcript: Arc::new(Mutex::new(SessionTranscript::default())),
            turn_id: 1,
        };

        // Act
        append_review_request_sync_warning(
            &input,
            SessionError::Workflow("metadata provider unavailable".to_string()),
        )
        .await;
        let messages = db
            .sessions()
            .load_session_messages("session-id")
            .await
            .expect("failed to load session messages");

        // Assert
        let warning = messages
            .iter()
            .find(|message| {
                message.entry_key.as_deref() == Some("branch_push:operation-id:review_request_sync")
            })
            .expect("missing keyed review-request warning");
        assert_eq!(warning.turn_id, 1);
        assert_eq!(warning.state, SessionMessageState::Failed.as_str());
    }
}
