//! Merges a session's linked pull request on its forge.
//!
//! The foreground admits a merge only while the session's latest refresh
//! reports the pull request ready, or ready with bypass when the caller
//! asked to bypass, and no other branch operation of the session runs. The
//! session then waits in `Queued`, and a background task holds the
//! session's branch-operation lock, asks the forge
//! to recheck and merge, refreshes the pull request, and reports back
//! through [`AppEvent::ReviewRequestMergeCompleted`]; the reducer then
//! records the outcome in the transcript and applies the refresh as the
//! background review-request sync does.

use std::path::PathBuf;
use std::sync::Arc;

use ag_forge::{
    MergeReviewRequestInput, ReviewRequestClient, ReviewRequestMergeMode, ReviewRequestMergeOutcome,
};
use ag_git::GitClient;
use ag_session::{ReviewRequestMerge, SessionError};
use tokio::sync::{OwnedMutexGuard, mpsc, oneshot};

use crate::app::core::{ReviewRequestStatusUpdate, SyncReviewRequestTaskResult};
use crate::app::session::{SessionTaskService, StatusTransition};
use crate::app::sync::{review_request_remote, review_request_sync_result};
use crate::app::{App, AppEvent, spawn};
use crate::domain::session::{
    ReviewRequest, ReviewRequestMergeAction, ReviewRequestMergeBlocker, SessionId, Status,
};
use crate::domain::transcript_notice::TranscriptNotice;

/// Refusal of a bypass for a pull request that merges without one.
const BYPASS_NOT_NEEDED: &str =
    "The session's pull request is ready to merge without bypassing branch protections";
/// Refusal of a merge without bypass for a pull request that needs one.
const BYPASS_REQUIRED: &str = "The session's pull request merges only by bypassing branch \
                               protections; confirm the bypass to merge it";
/// Refusal while another branch operation of the session holds its lock.
const BRANCH_OPERATION_RUNNING: &str =
    "Another branch operation of the session is running; merge its pull request once it finishes";

/// Reply channel of a merge whose caller waits for the forge's answer.
type MergeReply = oneshot::Sender<Result<ReviewRequestMerge, SessionError>>;

/// Finished merge of one session's pull request, applied by the reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewRequestMergeCompletion {
    /// Display id of the merged pull request, such as `#42`.
    pub(crate) display_id: String,
    /// What the forge did, or why the merge failed.
    pub(crate) outcome: Result<ReviewRequestMergeOutcome, String>,
    /// The pull request refreshed after the attempt, as the background
    /// review-request sync reports it; `None` when the refresh failed.
    pub(crate) refreshed: Option<SyncReviewRequestTaskResult>,
    /// Short forge name of the pull request, such as `PR`.
    pub(crate) short_name: &'static str,
    /// Branch the pull request merges into.
    pub(crate) target_branch: String,
}

impl ReviewRequestMergeCompletion {
    /// Returns the durable transcript notice for this merge.
    pub(crate) fn notice(&self) -> String {
        let pull_request = format!("{} {}", self.short_name, self.display_id);
        let target_branch = &self.target_branch;

        match &self.outcome {
            Ok(ReviewRequestMergeOutcome::Merged {
                merge_commit: Some(merge_commit),
            }) => TranscriptNotice::ReviewRequest.format(format!(
                "Merged {pull_request} into `{target_branch}` as `{}`",
                merge_commit.get(..7).unwrap_or(merge_commit)
            )),
            Ok(ReviewRequestMergeOutcome::Merged { merge_commit: None }) => {
                TranscriptNotice::ReviewRequest
                    .format(format!("Merged {pull_request} into `{target_branch}`"))
            }
            Ok(ReviewRequestMergeOutcome::Queued) => TranscriptNotice::ReviewRequest.format(
                format!("Added {pull_request} to the merge queue of `{target_branch}`"),
            ),
            Ok(ReviewRequestMergeOutcome::Refused { reason }) => TranscriptNotice::MergeError
                .format(format!("Did not merge {pull_request}: {reason}")),
            Err(message) => TranscriptNotice::MergeError
                .format(format!("Could not merge {pull_request}: {message}")),
        }
    }

    /// Returns whether the session stays in `Queued` once this merge
    /// settles: its refreshed pull request waits in the merge queue, or,
    /// without a refresh, the forge reported it merged or queued, which the
    /// next background refresh settles.
    pub(crate) fn leaves_session_queued(&self) -> bool {
        match self
            .refreshed
            .as_ref()
            .and_then(|refreshed| refreshed.summary.as_ref())
        {
            Some(summary) => summary.is_github_pr_in_merge_queue(),
            None => matches!(
                self.outcome,
                Ok(ReviewRequestMergeOutcome::Merged { .. } | ReviewRequestMergeOutcome::Queued)
            ),
        }
    }

    /// Returns the answer for a caller that waits for the merge.
    fn reply(&self) -> Result<ReviewRequestMerge, SessionError> {
        let outcome = self.outcome.clone().map_err(SessionError::Operation)?;

        Ok(ReviewRequestMerge {
            outcome,
            review_request: self
                .refreshed
                .as_ref()
                .and_then(|refreshed| refreshed.summary.clone()),
        })
    }
}

impl App {
    /// Admits the merge of `session_id`'s pull request, bypassing branch
    /// protections only when `bypass`, and starts it in the background.
    ///
    /// The merge must match the readiness the latest refresh reported, and
    /// waits for nothing: a session with other work, a publish, or a merge
    /// of its own running or queued is refused. A refusal answers on
    /// `response_tx` when the caller waits for one, and is returned
    /// otherwise; an admitted merge answers there once the forge did.
    ///
    /// The session waits in `Queued` while the merge runs, so no reply,
    /// sync, publish, or review-watch follow-up starts on a branch that is
    /// about to merge.
    pub(crate) async fn start_review_request_merge(
        &mut self,
        session_id: SessionId,
        bypass: bool,
        response_tx: Option<MergeReply>,
    ) -> Result<(), SessionError> {
        let (task, queued_transition) = match self.review_request_merge_task(&session_id, bypass) {
            Ok(admitted) => admitted,
            Err(refusal) => {
                let Some(response_tx) = response_tx else {
                    return Err(refusal);
                };
                let _ = response_tx.send(Err(refusal));

                return Ok(());
            }
        };

        // Recorded app-wide, not on the session's page, so no reload or
        // project switch releases the session while the merge runs.
        self.review_request_merges.insert(session_id.clone());
        // `Review` and `Watching`, the only statuses a merge starts from,
        // always move to `Queued`.
        let _ = queued_transition.apply(Status::Queued).await;
        self.sessions
            .start_review_request_merge(&session_id, task.loading_label());
        self.mark_dirty();
        spawn::spawn_detached(task.run(session_id, response_tx));

        Ok(())
    }

    /// Applies one finished merge: records its outcome in the transcript,
    /// then applies the refreshed pull request as the background
    /// review-request sync does, so a merged one moves its session to
    /// `Merged` and a queued one stays `Queued`. Any other outcome returns
    /// the session to `Review` or `Watching`, on whichever page it is
    /// loaded.
    pub(in crate::app) async fn apply_review_request_merge_completion(
        &mut self,
        session_id: SessionId,
        completion: ReviewRequestMergeCompletion,
    ) {
        self.review_request_merges.remove(&session_id);
        let stays_queued = completion.leaves_session_queued();
        let notice = completion.notice();
        if self
            .sessions
            .finish_review_request_merge(&session_id, &notice)
        {
            SessionTaskService::persist_workflow_notice(self.services.db(), &session_id, &notice)
                .await;
        }
        if let Some(refreshed) = completion.refreshed {
            self.apply_review_request_status_update(ReviewRequestStatusUpdate {
                generation: self.sync_handle.current_generation(),
                result: Ok(refreshed),
                session_id: session_id.clone(),
            })
            .await;
            self.publish_sync_context();
        }
        if !stays_queued {
            self.release_queued_session(&session_id).await;
        }
    }

    /// Returns a session still held in `Queued` by its merge to `Watching`
    /// when it is watched, or to `Review`, through its handle, so the
    /// release reaches a session on any loaded page.
    async fn release_queued_session(&self, session_id: &SessionId) {
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };
        if !handles
            .status
            .lock()
            .is_ok_and(|status| *status == Status::Queued)
        {
            return;
        }
        let idle_status = if self.review_watch.is_watched(session_id) {
            Status::Watching
        } else {
            Status::Review
        };

        // Best-effort, as `Queued` always returns to `Review` or `Watching`.
        let _ = StatusTransition::from_services(&self.services, handles, session_id.clone())
            .apply(idle_status)
            .await;
    }

    /// Checks that `session_id`'s pull request may merge now and builds the
    /// background merge, holding the session's branch-operation lock, with
    /// the transition that holds the session while it runs.
    fn review_request_merge_task(
        &self,
        session_id: &SessionId,
        bypass: bool,
    ) -> Result<(ReviewRequestMergeTask, StatusTransition), SessionError> {
        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(SessionError::NotFound)?;
        let action = session
            .review_request_merge_availability()
            .map_err(|blocker| SessionError::Operation(blocker.message().to_string()))?;
        let mode = match (action, bypass) {
            (ReviewRequestMergeAction::Merge, false) => ReviewRequestMergeMode::Ready,
            (ReviewRequestMergeAction::BypassMerge, true) => ReviewRequestMergeMode::Bypass,
            (ReviewRequestMergeAction::Merge, true) => {
                return Err(SessionError::Operation(BYPASS_NOT_NEEDED.to_string()));
            }
            (ReviewRequestMergeAction::BypassMerge, false) => {
                return Err(SessionError::Operation(BYPASS_REQUIRED.to_string()));
            }
        };
        if self.merge_queue.is_queued_or_active(session_id) {
            return Err(SessionError::Operation(
                ReviewRequestMergeBlocker::Busy.message().to_string(),
            ));
        }
        let handles = self
            .sessions
            .session_handles_or_err(session_id)
            .ok()
            .ok_or(SessionError::NotFound)?;
        let branch_operation_guard = handles
            .branch_operation_lock
            .clone()
            .try_lock_owned()
            .map_err(|_| SessionError::Operation(BRANCH_OPERATION_RUNNING.to_string()))?;
        let linked_review_request = session
            .review_request
            .clone()
            .ok_or(SessionError::NotFound)?;
        let task = ReviewRequestMergeTask {
            app_event_tx: self.services.event_sender(),
            branch_operation_guard,
            folder: session.folder.clone(),
            git_client: self.services.git_client(),
            linked_review_request,
            mode,
            review_request_client: self.services.review_request_client(),
        };

        Ok((
            task,
            StatusTransition::from_services(&self.services, handles, session_id.clone()),
        ))
    }
}

/// One admitted merge, run in the background while it holds the session's
/// branch-operation lock.
pub(crate) struct ReviewRequestMergeTask {
    /// Event sink for the completion.
    pub(crate) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Keeps publishing, local merges, and pushes of the session out until
    /// the merge and its refresh finish.
    pub(crate) branch_operation_guard: OwnedMutexGuard<()>,
    /// Session worktree whose remote and head the merge uses.
    pub(crate) folder: PathBuf,
    /// Reads the session branch's head commit.
    pub(crate) git_client: Arc<dyn GitClient>,
    /// The pull request the admission checked.
    pub(crate) linked_review_request: ReviewRequest,
    /// Readiness the merge requires.
    pub(crate) mode: ReviewRequestMergeMode,
    /// Forge client that merges and refreshes the pull request.
    pub(crate) review_request_client: Arc<dyn ReviewRequestClient>,
}

impl ReviewRequestMergeTask {
    /// Returns the inline loading label shown while the merge runs.
    pub(crate) fn loading_label(&self) -> String {
        let summary = &self.linked_review_request.summary;
        let pull_request = format!(
            "{} {}",
            summary.forge_kind.review_request_short_name(),
            summary.display_id
        );

        match self.mode {
            ReviewRequestMergeMode::Bypass => format!(
                "Merging {pull_request} into `{}` by bypassing branch protections...",
                summary.target_branch
            ),
            ReviewRequestMergeMode::Ready => {
                format!("Merging {pull_request} into `{}`...", summary.target_branch)
            }
        }
    }

    /// Merges and refreshes the pull request, then reports the completion
    /// to the reducer and to a caller that waits on `response_tx`.
    pub(crate) async fn run(self, session_id: SessionId, response_tx: Option<MergeReply>) {
        let completion = self.merge().await;
        let reply = completion.reply();
        // Fire-and-forget: the receiver may be dropped during shutdown.
        let _ = self
            .app_event_tx
            .send(AppEvent::ReviewRequestMergeCompleted {
                completion: Box::new(completion),
                session_id,
            });
        drop(self.branch_operation_guard);
        if let Some(response_tx) = response_tx {
            let _ = response_tx.send(reply);
        }
    }

    /// Asks the forge to merge the pull request at the session branch's
    /// head, then refreshes it whether or not the merge succeeded, since a
    /// draft marked ready for review stays ready after a failure.
    async fn merge(&self) -> ReviewRequestMergeCompletion {
        let summary = &self.linked_review_request.summary;
        let completion = |outcome, refreshed| ReviewRequestMergeCompletion {
            display_id: summary.display_id.clone(),
            outcome,
            refreshed,
            short_name: summary.forge_kind.review_request_short_name(),
            target_branch: summary.target_branch.clone(),
        };
        let remote = match review_request_remote(
            self.folder.clone(),
            self.git_client.as_ref(),
            Some(&self.linked_review_request),
            self.review_request_client.as_ref(),
        )
        .await
        {
            Ok(remote) => remote,
            Err(message) => return completion(Err(message), None),
        };
        let outcome = match self.git_client.head_hash(self.folder.clone()).await {
            Ok(head_commit) => self
                .review_request_client
                .merge_review_request(
                    remote.clone(),
                    MergeReviewRequestInput {
                        display_id: summary.display_id.clone(),
                        expected_head_commit: Some(head_commit),
                        mode: self.mode,
                    },
                )
                .await
                .map_err(|error| error.detail_message()),
            Err(error) => Err(format!("Could not read the session branch head: {error}")),
        };
        let refreshed = match self
            .review_request_client
            .refresh_review_request(remote, summary.display_id.clone())
            .await
        {
            Ok(refreshed_summary) => Some(
                review_request_sync_result(
                    self.git_client.as_ref(),
                    &self.folder,
                    refreshed_summary,
                )
                .await,
            ),
            Err(_) => None,
        };

        completion(outcome, refreshed)
    }
}

#[cfg(test)]
#[path = "review_request_merge_test.rs"]
mod tests;
