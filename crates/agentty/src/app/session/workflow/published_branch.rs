//! Published-branch post-turn synchronization for session workers.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use ag_agent::OneShotClient;
use ag_forge as forge;
use ag_git::GitClient;
use ag_protocol::{ReviewCommentOutcome, ReviewCommentResolution};
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
use crate::infra::db::AppRepositories;

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
    /// Valid review-thread outcomes eligible for post-push forge updates.
    pub(super) review_comment_outcomes: Vec<ReviewCommentOutcome>,
    /// Forge boundary used for optional linked PR/MR metadata refresh.
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Optional auto-commit message used to refresh linked PR/MR metadata.
    pub(super) review_request_commit_message: Option<String>,
    /// Agent/model selection used for metadata reconciliation.
    pub(super) session_agent: AgentSelection,
    /// Session id whose branch is being pushed.
    pub(super) session_id: SessionId,
    /// Cumulative session summary used to reconcile review-request metadata.
    pub(super) session_summary: Option<String>,
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
                    session_summary: input.session_summary.unwrap_or_default(),
                },
                review_request_client: Arc::clone(&input.review_request_client),
            });
    let review_comment_resolution =
        (!input.review_comment_outcomes.is_empty()).then(|| ReviewCommentResolutionInput {
            outcomes: input.review_comment_outcomes,
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
        review_comment_resolution,
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
    /// Optional review-thread outcomes applied only after a successful push.
    pub(super) review_comment_resolution: Option<ReviewCommentResolutionInput>,
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
    /// Cumulative session summary used to evaluate material metadata changes.
    pub(super) session_summary: String,
}

/// Owned dependencies for forge thread replies and resolution after push.
pub(super) struct ReviewCommentResolutionInput {
    /// Valid, allowlisted outcomes reported by the completed agent turn.
    pub(super) outcomes: Vec<ReviewCommentOutcome>,
    /// Forge boundary used to post replies and resolve review threads.
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
            if let Some(resolution_input) = input.review_comment_resolution.as_ref() {
                resolve_review_comments_after_push(&input, resolution_input).await;
            }

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
async fn resolve_review_comments_after_push(
    input: &PublishedBranchAutoPushInput,
    resolution_input: &ReviewCommentResolutionInput,
) {
    let expected_reply_count = resolution_input.outcomes.len();
    let expected_resolution_count = resolution_input
        .outcomes
        .iter()
        .filter(|outcome| outcome.resolution == ReviewCommentResolution::Fixed)
        .count();
    let linked_review_request = match load_open_review_request(input).await {
        Ok(Some(linked_review_request)) => linked_review_request,
        Ok(None) => return,
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

            return;
        }
    };
    let remote = match resolution_input
        .review_request_client
        .detect_remote(repo_url)
        .map(|remote| remote.with_command_working_directory(input.folder.clone()))
    {
        Ok(remote) => remote,
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

            return;
        }
    };

    let (replied_count, resolved_count) = post_review_comment_outcomes(
        input,
        resolution_input,
        &remote,
        &linked_review_request.summary.display_id,
    )
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

/// Posts each accepted reply and resolves only fixed outcomes.
async fn post_review_comment_outcomes(
    input: &PublishedBranchAutoPushInput,
    resolution_input: &ReviewCommentResolutionInput,
    remote: &forge::ForgeRemote,
    display_id: &str,
) -> (usize, usize) {
    let mut replied_count = 0;
    let mut resolved_count = 0;

    for outcome in &resolution_input.outcomes {
        let reply_result = resolution_input
            .review_request_client
            .reply_to_thread(
                remote.clone(),
                display_id.to_string(),
                outcome.thread_id.clone(),
                outcome.reply.clone(),
            )
            .await;
        if let Err(error) = reply_result {
            let error_detail = error.detail_message();
            tracing::warn!(
                session_id = %input.session_id,
                thread_id = %outcome.thread_id,
                error = %error_detail,
                "failed to reply to review thread"
            );

            continue;
        }
        replied_count += 1;
        if outcome.resolution == ReviewCommentResolution::NoChangeNeeded {
            continue;
        }

        let resolve_result = resolution_input
            .review_request_client
            .resolve_thread(
                remote.clone(),
                display_id.to_string(),
                outcome.thread_id.clone(),
            )
            .await;
        match resolve_result {
            Ok(()) => resolved_count += 1,
            Err(error) => {
                let error_detail = error.detail_message();
                tracing::warn!(
                    session_id = %input.session_id,
                    thread_id = %outcome.thread_id,
                    error = %error_detail,
                    "failed to resolve replied review thread"
                );
            }
        }
    }

    (replied_count, resolved_count)
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
                 resolved {resolved_count} of {expected_resolution_count} fixed thread(s). Reopen \
                 review comments to retry the remaining threads."
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
        &evaluation.session_summary,
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
    use ag_forge::MockReviewRequestClient;
    use ag_git::{GitError, MockGitClient};

    use super::*;

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
                    crate::domain::agent::AgentModel::Gpt55,
                ),
                session_summary: "The same goal now includes the completed body.".to_string(),
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
        );
        let resolution_input = input
            .review_comment_resolution
            .as_ref()
            .expect("resolution input should exist");

        // Act
        resolve_review_comments_after_push(&input, resolution_input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 1 of 2 review thread(s) and resolved 0 of 2 \
             fixed thread(s). Reopen review comments to retry the remaining threads."
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
            .expect_reply_to_thread()
            .withf(|_, _, thread_id, body| {
                (thread_id == "no-change" && body == "The current implementation is already safe.")
                    || (thread_id == "fixed" && body == "Addressed fixed.")
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
        );
        let resolution_input = input
            .review_comment_resolution
            .as_ref()
            .expect("resolution input should exist");

        // Act
        resolve_review_comments_after_push(&input, resolution_input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments] Replied to 2 review thread(s) and resolved 1 fixed thread(s)."
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
        );
        let resolution_input = input
            .review_comment_resolution
            .as_ref()
            .expect("resolution input should exist");

        // Act
        resolve_review_comments_after_push(&input, resolution_input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 \
             fixed thread(s). Reopen review comments to retry the remaining threads."
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
        );
        let resolution_input = input
            .review_comment_resolution
            .as_ref()
            .expect("resolution input should exist");

        // Act
        resolve_review_comments_after_push(&input, resolution_input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 \
             fixed thread(s). Reopen review comments to retry the remaining threads."
        );
    }

    #[tokio::test]
    async fn review_comment_resolution_skips_sessions_without_open_linked_review() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        insert_session(&db).await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().never();
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            MockReviewRequestClient::new(),
            vec![fixed_outcome("thread-1")],
        );
        let resolution_input = input
            .review_comment_resolution
            .as_ref()
            .expect("resolution input should exist");

        // Act
        resolve_review_comments_after_push(&input, resolution_input).await;

        // Assert
        assert!(transcript.lock().expect("transcript lock").is_empty());
    }

    #[tokio::test]
    async fn review_comment_resolution_reports_linked_review_load_failure() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool().await;
        insert_session(&db).await;
        pool.close().await;
        let mut git_client = MockGitClient::new();
        git_client.expect_repo_url().never();
        let (input, transcript) = resolution_test_input(
            db,
            git_client,
            MockReviewRequestClient::new(),
            vec![fixed_outcome("thread-1")],
        );
        let resolution_input = input
            .review_comment_resolution
            .as_ref()
            .expect("resolution input should exist");

        // Act
        resolve_review_comments_after_push(&input, resolution_input).await;

        // Assert
        assert_eq!(
            last_transcript_message(&transcript),
            "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 \
             fixed thread(s). Reopen review comments to retry the remaining threads."
        );
    }

    /// Builds one detached-push input for direct review-resolution tests.
    fn resolution_test_input(
        db: AppRepositories,
        git_client: MockGitClient,
        review_request_client: MockReviewRequestClient,
        outcomes: Vec<ReviewCommentOutcome>,
    ) -> (PublishedBranchAutoPushInput, Arc<Mutex<SessionTranscript>>) {
        let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
        let input = PublishedBranchAutoPushInput {
            app_event_tx: mpsc::unbounded_channel().0,
            db,
            folder: PathBuf::from("/tmp/project"),
            git_client: Arc::new(git_client),
            published_upstream_ref: "origin/wt/session-id".to_string(),
            review_comment_resolution: Some(ReviewCommentResolutionInput {
                outcomes,
                review_request_client: Arc::new(review_request_client),
            }),
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
            review_comment_resolution: None,
            review_request_metadata_sync: None,
            session_id: "session-id".into(),
            session_update_versions: Arc::default(),
            sync_operation_id: "sync-id".to_string(),
            transcript: Arc::clone(&transcript),
        };

        (input, transcript)
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

    /// Inserts one session linked to an open GitHub pull request.
    async fn linked_review_request_db() -> AppRepositories {
        let db = AppRepositories::in_memory().await;
        insert_session(&db).await;
        db.reviews()
            .update_session_review_request(
                "session-id",
                Some(ReviewRequest {
                    last_refreshed_at: 100,
                    summary: forge::ReviewRequestSummary {
                        display_id: "#42".to_string(),
                        forge_kind: forge::ForgeKind::GitHub,
                        source_branch: "wt/session-id".to_string(),
                        state: ReviewRequestState::Open,
                        status_summary: None,
                        target_branch: "main".to_string(),
                        title: "Review title".to_string(),
                        web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                    },
                }),
            )
            .await
            .expect("failed to link review request");

        db
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
                "gemini-3-flash-preview",
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
