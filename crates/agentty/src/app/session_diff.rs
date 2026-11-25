//! Background full-diff request tracking and reducer-owned completion handling.

use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ag_contracts::SessionStats;
use ag_git::DiffFile;
use ag_session::{SessionDiffFile, SessionDiffFileStatus, SessionError as ApiSessionError};
use tokio::sync::oneshot;
use tracing::warn;

use crate::app::review::{self, FocusedReviewPersistence, ReviewAgent, ReviewCacheEntry};
use crate::app::task::{SessionDiffTaskInput, SessionDiffTaskSource, TaskService};
use crate::app::{App, session, spawn};
use crate::domain::agent::parse_persisted_session_agent_model;
use crate::domain::review::FocusedReviewStatus;
use crate::domain::session::{Session, SessionId, Status};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::infra::db::DbError;
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, DiffRestoreTarget, DiffSidebarFocus,
};

/// Maximum number of delayed persistence attempts after an automatic-review
/// deferral write fails.
const MAX_DEFERRED_AUTO_REVIEW_PERSISTENCE_RETRIES: u8 = 3;

/// Time between background re-reads of the full diff shown on an open Diff
/// page.
const OPEN_DIFF_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// One delayed automatic-review deferral persistence attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeferredAutoReviewPersistenceRetry {
    /// One-based delayed retry number.
    pub(crate) attempt: u8,
    /// Session whose automatic focused review remains deferred.
    pub(crate) session_id: SessionId,
}

impl DeferredAutoReviewPersistenceRetry {
    /// Wraps one initial write before any delayed retries have run.
    fn initial(session_id: SessionId) -> Self {
        Self {
            attempt: 0,
            session_id,
        }
    }

    /// Returns the next bounded retry, or `None` after the retry limit.
    fn next(self) -> Option<Self> {
        (self.attempt < MAX_DEFERRED_AUTO_REVIEW_PERSISTENCE_RETRIES).then(|| Self {
            attempt: self.attempt.saturating_add(1),
            session_id: self.session_id,
        })
    }
}

/// Completed session-diff task ready for stale-safe reducer application.
pub(crate) struct SessionDiffUpdate {
    /// Request generation assigned when the background task started.
    pub(crate) request_id: u64,
    /// Full diff text or the normalized load failure.
    pub(crate) result: Result<String, String>,
    /// Session whose pending continuation owns this result.
    pub(crate) session_id: SessionId,
}

/// Foreground continuation waiting on one background diff result.
pub(crate) struct PendingSessionDiffRequest {
    purpose: SessionDiffPurpose,
    session_id: SessionId,
}

/// Periodic live-refresh state for the full diff shown on an open Diff page.
#[derive(Default)]
pub(crate) struct OpenDiffRefresh {
    /// Session whose open diff is scheduled and the instant its next
    /// background re-read is due.
    deadline: Option<(SessionId, Instant)>,
    /// Newer diff waiting for the runtime to apply it to the open page.
    refreshed: Option<RefreshedSessionDiff>,
}

/// Newer full diff text loaded for one open Diff page.
pub(crate) struct RefreshedSessionDiff {
    /// Complete replacement diff text.
    pub(crate) diff: String,
    /// Session whose open Diff page requested this diff.
    pub(crate) session_id: SessionId,
}

/// Stable focused-review inputs captured before a project switch can unload
/// the session snapshot.
struct FocusedReviewTarget {
    folder: PathBuf,
    review_agent: ReviewAgent,
}

/// Action resumed after one full diff finishes loading.
enum SessionDiffPurpose {
    ApplyFocusedReview {
        auto_address: bool,
        cached_diff_hash: u64,
        suggestions: String,
    },
    Open,
    Refresh,
    Review {
        cached_diff_hash: Option<u64>,
        is_manual: bool,
        target: FocusedReviewTarget,
    },
}

impl SessionDiffPurpose {
    /// Returns whether the request is validating a focused-review apply.
    fn is_apply_focused_review(&self) -> bool {
        matches!(self, Self::ApplyFocusedReview { .. })
    }

    /// Returns whether the request belongs to a focused-review continuation.
    fn is_review_action(&self) -> bool {
        matches!(self, Self::ApplyFocusedReview { .. } | Self::Review { .. })
    }
}

impl App {
    /// Starts loading a full diff and immediately switches to a cancelable
    /// loading page, preserving any composer or question restore state.
    pub(crate) fn start_diff_view_load(
        &mut self,
        session_id: &SessionId,
        restore: Option<DiffRestoreTarget>,
        sidebar_focus: DiffSidebarFocus,
    ) -> bool {
        let fallback_view_scroll_offset = match &self.mode {
            AppMode::View {
                scroll_offset,
                session_id: viewed_session_id,
            } if viewed_session_id == session_id => *scroll_offset,
            _ => None,
        };
        self.discard_open_diff_refresh(session_id);
        let Some(request_id) =
            self.spawn_session_diff_request(session_id, SessionDiffPurpose::Open)
        else {
            if let Some(restore) = restore {
                self.mode = restore.into_mode();
            }

            return false;
        };

        self.mode = AppMode::DiffLoading {
            fallback_view_scroll_offset,
            request_id,
            restore: restore.map(Box::new),
            session_id: session_id.clone(),
            sidebar_focus,
        };

        true
    }

    /// Cancels a still-current interactive diff load and restores its source
    /// page. The detached Git task may finish, but its event is then stale.
    pub(crate) fn cancel_diff_view_load(&mut self) {
        let mode = std::mem::replace(&mut self.mode, AppMode::List);
        let AppMode::DiffLoading {
            fallback_view_scroll_offset,
            request_id,
            restore,
            session_id,
            ..
        } = mode
        else {
            self.mode = mode;

            return;
        };

        self.pending_session_diff_requests.remove(&request_id);
        self.mode = restore.map_or(
            AppMode::View {
                scroll_offset: fallback_view_scroll_offset,
                session_id,
            },
            |restore| restore.into_mode(),
        );
    }

    /// Starts one background re-read of the open Diff page's full diff each
    /// time its refresh interval elapses, so worktree changes made while the
    /// page stays open appear without reopening it. Archived and unstarted
    /// sessions are skipped because their diff cannot change.
    pub(crate) fn start_open_diff_refresh_if_due(&mut self, now: Instant) {
        let AppMode::Diff { session_id, .. } = &self.mode else {
            self.open_diff_refresh.deadline = None;

            return;
        };
        let session_id = session_id.clone();
        let next_deadline = Some((session_id.clone(), now + OPEN_DIFF_REFRESH_INTERVAL));
        match &self.open_diff_refresh.deadline {
            Some((scheduled_session_id, deadline)) if *scheduled_session_id == session_id => {
                if now < *deadline {
                    return;
                }
            }
            Some(_) | None => {
                self.open_diff_refresh.deadline = next_deadline;

                return;
            }
        }

        self.open_diff_refresh.deadline = next_deadline;
        let is_refresh_pending = self.pending_session_diff_requests.values().any(|request| {
            request.session_id == session_id
                && matches!(request.purpose, SessionDiffPurpose::Refresh)
        });
        if is_refresh_pending {
            return;
        }
        let Some(session) = self.sessions.session_for_id(&session_id) else {
            return;
        };
        let (folder, source) = self.session_diff_task_target(session);
        if !matches!(source, SessionDiffTaskSource::Worktree { .. }) {
            return;
        }

        self.spawn_session_diff_request_for_target(
            &session_id,
            SessionDiffPurpose::Refresh,
            folder,
            source,
        );
    }

    /// Takes the newer diff waiting to replace the open Diff page's content.
    pub(crate) fn take_refreshed_session_diff(&mut self) -> Option<RefreshedSessionDiff> {
        self.open_diff_refresh.refreshed.take()
    }

    /// Holds a newer diff for `session_id` until the runtime can apply it to
    /// the open page. A later refresh replaces it.
    pub(crate) fn queue_refreshed_session_diff(&mut self, session_id: SessionId, diff: String) {
        self.open_diff_refresh.refreshed = Some(RefreshedSessionDiff { diff, session_id });
    }

    /// Discards every diff continuation, deferred automatic-review trigger,
    /// and deferred setting change owned by a deleted session so detached
    /// task completions remain stale.
    pub(crate) fn discard_deleted_session_diff_state(&mut self, session_id: &SessionId) {
        self.auto_address_review_iterations.remove(session_id);
        self.review_diff_hashes.remove(session_id);
        self.diff_comment_progress.remove(session_id);
        self.pending_session_diff_requests
            .retain(|_, request| request.session_id != *session_id);
        self.deferred_auto_review_session_ids.remove(session_id);
        self.deferred_session_settings.remove(session_id);
        self.role_review_suppression.remove(session_id);
    }

    /// Saves completed diff comments while their session's Diff mode is
    /// closed. Interaction-only selection state is reset before restoration.
    pub(crate) fn save_diff_comment_progress(
        &mut self,
        session_id: SessionId,
        mut line_comments: DiffLineComments,
    ) {
        line_comments.editing_index = None;
        line_comments.selection_anchor_index = None;
        line_comments.selected_comment_index = None;
        line_comments.submit_confirmation_index = None;
        if line_comments.comments.is_empty() {
            self.diff_comment_progress.remove(&session_id);

            return;
        }

        self.diff_comment_progress.insert(session_id, line_comments);
    }

    /// Clears saved and currently displayed diff comments for one session
    /// after a new turn starts.
    pub(crate) fn clear_diff_comment_progress(&mut self, session_id: &str) {
        self.diff_comment_progress.remove(session_id);
        match &mut self.mode {
            AppMode::Diff {
                line_comments,
                scroll_cache,
                session_id: diff_session_id,
                ..
            } if diff_session_id == session_id => {
                *line_comments = DiffLineComments::default();
                *scroll_cache = None;
            }
            AppMode::Help {
                context:
                    crate::presentation::app_mode::HelpContext::Diff {
                        line_comments,
                        session_id: diff_session_id,
                        ..
                    },
                ..
            } if diff_session_id == session_id => {
                *line_comments = DiffLineComments::default();
            }
            _ => {}
        }
    }

    /// Persists and retains an automatic-review trigger for an eligible
    /// session that cannot start its review yet.
    pub(super) async fn defer_auto_review_session(&mut self, session_id: &SessionId) {
        self.persist_deferred_auto_review(DeferredAutoReviewPersistenceRetry::initial(
            session_id.clone(),
        ))
        .await;
    }

    /// Retries current automatic-review deferral writes after their bounded
    /// backoff delay.
    pub(super) async fn persist_deferred_auto_review_retries(
        &mut self,
        retries: Vec<DeferredAutoReviewPersistenceRetry>,
    ) {
        for retry in retries {
            if self
                .deferred_auto_review_session_ids
                .contains(&retry.session_id)
            {
                self.persist_deferred_auto_review(retry).await;
            }
        }
    }

    /// Applies one automatic-review deferral persistence attempt.
    async fn persist_deferred_auto_review(&mut self, retry: DeferredAutoReviewPersistenceRetry) {
        let result = self
            .services
            .db()
            .sessions()
            .defer_session_focused_review(retry.session_id.as_str())
            .await;
        Self::handle_deferred_auto_review_persistence_result(
            &mut self.deferred_auto_review_session_ids,
            self.services.event_sender(),
            retry,
            result,
        );
    }

    /// Retains failed triggers and schedules their next bounded persistence
    /// attempt through the foreground event reducer.
    fn handle_deferred_auto_review_persistence_result(
        deferred_session_ids: &mut HashSet<SessionId>,
        app_event_tx: tokio::sync::mpsc::UnboundedSender<crate::app::AppEvent>,
        retry: DeferredAutoReviewPersistenceRetry,
        result: Result<bool, DbError>,
    ) -> bool {
        let session_id = retry.session_id.clone();
        match result {
            Ok(true) => {
                deferred_session_ids.insert(session_id);

                false
            }
            Ok(false) => {
                deferred_session_ids.remove(&session_id);

                false
            }
            Err(error) => {
                deferred_session_ids.insert(session_id.clone());
                let Some(retry) = retry.next() else {
                    warn!(
                        session_id = %session_id,
                        error = %error,
                        "deferred automatic focused-review persistence retries exhausted; \
                         retaining the in-memory trigger"
                    );

                    return false;
                };
                warn!(
                    session_id = %session_id,
                    retry_attempt = retry.attempt,
                    %error,
                    "failed to persist deferred automatic focused review; scheduling retry"
                );
                TaskService::spawn_deferred_auto_review_persistence_retry(app_event_tx, retry);

                true
            }
        }
    }

    /// Starts one manual focused-review diff load unless that session already
    /// has a current request.
    pub(crate) fn start_manual_review_diff_load(&mut self, session_id: &SessionId) -> bool {
        let started = self.start_review_diff_load(session_id, true);
        if started {
            self.role_review_suppression.remove(session_id);
        }

        started
    }

    /// Starts one focused-review freshness check without blocking prompt
    /// input or redraws on the full Git diff.
    pub(crate) fn start_apply_review_diff_load(
        &mut self,
        session_id: &SessionId,
        cached_diff_hash: u64,
        suggestions: String,
    ) -> bool {
        self.start_apply_review_diff_load_with_mode(
            session_id,
            cached_diff_hash,
            suggestions,
            false,
        )
    }

    /// Starts one automatic focused-review freshness check.
    pub(crate) fn start_auto_apply_review_diff_load(
        &mut self,
        session_id: &SessionId,
        cached_diff_hash: u64,
        suggestions: String,
    ) -> bool {
        if self.role_review_suppression.contains_key(session_id) {
            return false;
        }
        self.start_apply_review_diff_load_with_mode(session_id, cached_diff_hash, suggestions, true)
    }

    fn start_apply_review_diff_load_with_mode(
        &mut self,
        session_id: &SessionId,
        cached_diff_hash: u64,
        suggestions: String,
        auto_address: bool,
    ) -> bool {
        if self.pending_session_diff_requests.values().any(|request| {
            request.session_id == *session_id && request.purpose.is_apply_focused_review()
        }) {
            return false;
        }

        self.spawn_session_diff_request(
            session_id,
            SessionDiffPurpose::ApplyFocusedReview {
                auto_address,
                cached_diff_hash,
                suggestions,
            },
        )
        .is_some()
    }

    /// Starts automatic review diff loads for eligible touched sessions on
    /// the active page, whose project's review agent applies.
    ///
    /// Requests are deduplicated per session, and existing generated output
    /// remains visible while its current diff hash is checked in the
    /// background.
    pub(super) fn start_auto_review_diff_loads(&mut self, session_ids: &HashSet<SessionId>) {
        for session_id in session_ids {
            if self.role_review_suppression.contains_key(session_id) {
                continue;
            }
            let Some(session) = self.sessions.active_page_session(session_id) else {
                continue;
            };
            let current_status = session.status;

            if matches!(
                self.review_cache.get(session_id),
                Some(ReviewCacheEntry::Suppressed)
            ) {
                continue;
            }

            if current_status == Status::InProgress {
                self.discard_pending_review_action_diff_loads(session_id);
                self.clear_review_output(session_id);

                continue;
            }
            if !current_status.allows_review_actions()
                || matches!(
                    self.review_cache.get(session_id),
                    Some(ReviewCacheEntry::Loading { .. })
                )
            {
                continue;
            }

            self.start_review_diff_load(session_id, false);
        }
    }

    /// Starts automatic review preparation for a review-ready session whose
    /// project is not currently loaded.
    pub(super) async fn start_inactive_auto_review_diff_load(
        &mut self,
        session_id: &SessionId,
    ) -> bool {
        if self.role_review_suppression.contains_key(session_id) {
            return false;
        }
        if self.pending_session_diff_requests.values().any(|request| {
            request.session_id == *session_id
                && matches!(&request.purpose, SessionDiffPurpose::Review { .. })
        }) || matches!(
            self.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { .. } | ReviewCacheEntry::Suppressed)
        ) {
            return true;
        }

        let Ok(Some(row)) = self
            .services
            .db()
            .sessions()
            .load_session(session_id.as_str())
            .await
        else {
            return false;
        };
        let status = row.status.parse::<Status>().ok();
        if !status.is_some_and(Status::allows_review_actions) {
            return false;
        }
        let Some(project_id) = row.project_id else {
            return false;
        };
        let review_agent = review::session_review_agent(
            crate::app::setting::load_default_review_agent_setting(&self.services, project_id)
                .await,
            parse_persisted_session_agent_model(Some(&row.agent), &row.model),
            &self.services.available_agent_kinds(),
        );
        let folder = session::session_folder(self.services.base_path(), session_id.as_str());
        let source = SessionDiffTaskSource::Worktree {
            archived_fallback: None,
            base_branch: row.base_branch,
            git_client: self.services.git_client(),
        };

        self.defer_auto_review_session(session_id).await;

        self.start_review_diff_load_for_target(
            session_id,
            false,
            FocusedReviewTarget {
                folder: folder.clone(),
                review_agent,
            },
            folder,
            source,
        )
    }

    /// Invalidates review and apply continuations captured before newly
    /// completed turns, then clears their stale focused-review generations.
    pub(super) fn supersede_review_diff_loads(&mut self, session_ids: &HashSet<SessionId>) {
        for session_id in session_ids {
            if self.role_review_suppression.contains_key(session_id) {
                continue;
            }
            self.discard_pending_review_action_diff_loads(session_id);
            self.clear_review_output(session_id);
            review::restore_session_review_status(self.sessions.state_mut(), session_id);
        }
    }

    /// Invalidates `/apply` diff continuations captured from review output
    /// that is being cleared or replaced.
    pub(super) fn discard_pending_apply_review_diff_loads(&mut self, session_id: &SessionId) {
        self.pending_session_diff_requests.retain(|_, request| {
            request.session_id != *session_id || !request.purpose.is_apply_focused_review()
        });
    }

    /// Applies one completed diff only when its request generation and
    /// session still match the foreground continuation.
    pub(crate) async fn apply_session_diff_update(&mut self, update: SessionDiffUpdate) {
        let request = match self.pending_session_diff_requests.entry(update.request_id) {
            Entry::Occupied(entry) if entry.get().session_id == update.session_id => entry.remove(),
            Entry::Occupied(_) | Entry::Vacant(_) => return,
        };

        match request.purpose {
            SessionDiffPurpose::ApplyFocusedReview {
                auto_address,
                cached_diff_hash,
                suggestions,
            } => {
                self.apply_focused_review_diff_update(
                    &update.session_id,
                    auto_address,
                    cached_diff_hash,
                    &suggestions,
                    update.result,
                )
                .await;
            }
            SessionDiffPurpose::Open => self.apply_open_diff_update(update),
            SessionDiffPurpose::Refresh => self.apply_refreshed_diff_update(update),
            SessionDiffPurpose::Review {
                cached_diff_hash,
                is_manual,
                target,
            } => {
                self.apply_review_diff_update(update, cached_diff_hash, is_manual, target)
                    .await;
            }
        }
    }

    /// Completes `/apply` only when the background diff still matches the
    /// focused-review generation selected by the user.
    async fn apply_focused_review_diff_update(
        &mut self,
        session_id: &SessionId,
        auto_address: bool,
        cached_diff_hash: u64,
        suggestions: &str,
        result: Result<String, String>,
    ) {
        let Some(current_review_text) =
            self.review_cache
                .get(session_id)
                .and_then(|entry| match entry {
                    ReviewCacheEntry::Ready {
                        diff_hash, text, ..
                    } if *diff_hash == cached_diff_hash => Some(text.clone()),
                    _ => None,
                })
        else {
            return;
        };
        let review_generation_is_current = self
            .sessions
            .session_for_id(session_id)
            .is_some_and(|session| matches!(session.status, Status::Review | Status::Watching));
        if !review_generation_is_current {
            return;
        }

        let current_diff = match result {
            Ok(diff) => diff,
            Err(error) => {
                self.append_prompt_status_line(
                    session_id,
                    crate::domain::transcript_notice::TranscriptNotice::Apply,
                    &format!(
                        "Failed to read worktree diff: {error}. Review cache preserved; try \
                         /apply again."
                    ),
                )
                .await;

                return;
            }
        };
        if review::diff_content_hash(&current_diff) != cached_diff_hash {
            self.clear_review_output(session_id);
            self.append_prompt_status_line(
                session_id,
                crate::domain::transcript_notice::TranscriptNotice::Apply,
                "Review is stale; the worktree changed since it was generated. Run focused review \
                 again (f key).",
            )
            .await;

            return;
        }

        let completed_auto_address_iterations = if auto_address {
            let auto_address_is_enabled = self.settings.auto_address_review_comments()
                && self.sessions.is_in_active_page(session_id);
            let completed_iterations = self
                .auto_address_review_iterations
                .get(session_id)
                .copied()
                .unwrap_or(0);
            if !auto_address_is_enabled
                || completed_iterations
                    >= crate::app::prompt_intent::MAX_AUTO_ADDRESS_REVIEW_ITERATIONS
            {
                return;
            }

            Some(completed_iterations)
        } else {
            None
        };

        let prompt = ag_session::build_apply_review_prompt(suggestions);
        if let Some(completed_iterations) = completed_auto_address_iterations {
            if !self.reply(session_id, prompt).await {
                let request_id = self.set_review_ready_output(
                    session_id,
                    cached_diff_hash,
                    current_review_text.clone(),
                );
                self.persist_focused_review_updates(vec![FocusedReviewPersistence {
                    generation_request_id: None,
                    request_id,
                    diff_hash: Some(cached_diff_hash),
                    session_id: session_id.clone(),
                    status: FocusedReviewStatus::Ready,
                    text: Some(current_review_text),
                }])
                .await;

                return;
            }

            self.auto_address_review_iterations
                .insert(session_id.clone(), completed_iterations.saturating_add(1));

            return;
        }

        self.reply(session_id, prompt).await;
    }

    /// Discards refresh reads and any queued refreshed diff left by an earlier
    /// Diff page, so their older results cannot replace a newly opened diff.
    fn discard_open_diff_refresh(&mut self, session_id: &SessionId) {
        self.pending_session_diff_requests.retain(|_, request| {
            request.session_id != *session_id
                || !matches!(request.purpose, SessionDiffPurpose::Refresh)
        });
        self.open_diff_refresh.refreshed = None;
    }

    /// Discards detached diff tasks whose continuations belong to an obsolete
    /// focused-review turn. Their eventual events are ignored as stale.
    fn discard_pending_review_action_diff_loads(&mut self, session_id: &SessionId) {
        self.pending_session_diff_requests.retain(|_, request| {
            request.session_id != *session_id || !request.purpose.is_review_action()
        });
    }

    /// Answers one programmatic diff query: captures the session's diff
    /// source on the foreground loop, then loads and splits the diff in a
    /// background task so Git never blocks the terminal. A session the
    /// host has not loaded answers `None`.
    pub(super) fn start_api_session_diff(
        &self,
        session_id: &SessionId,
        response_tx: oneshot::Sender<Result<Option<Vec<SessionDiffFile>>, ApiSessionError>>,
    ) {
        let Some(session) = self.sessions.session_for_id(session_id) else {
            let _ = response_tx.send(Ok(None));

            return;
        };
        let (folder, source) = self.session_diff_task_target(session);
        let session_id = session_id.clone();
        spawn::spawn_detached(async move {
            let result = TaskService::load_session_diff(folder, &session_id, source)
                .await
                .map(|diff| Some(api_diff_files(&diff)))
                .map_err(ApiSessionError::Operation);
            let _ = response_tx.send(result);
        });
    }

    /// Spawns the appropriate archive or worktree diff source and registers
    /// the continuation before the foreground task yields again.
    fn spawn_session_diff_request(
        &mut self,
        session_id: &SessionId,
        purpose: SessionDiffPurpose,
    ) -> Option<u64> {
        let session = self.sessions.session_for_id(session_id)?;
        let (folder, source) = self.session_diff_task_target(session);

        Some(self.spawn_session_diff_request_for_target(session_id, purpose, folder, source))
    }

    /// Captures one loaded session's folder and diff source: the archive for
    /// finished sessions, no changes for an unstarted session without a
    /// worktree, and the live worktree otherwise.
    fn session_diff_task_target(&self, session: &Session) -> (PathBuf, SessionDiffTaskSource) {
        let has_worktree = self
            .sessions
            .session_worktree_availability()
            .get(&session.id)
            .copied()
            .unwrap_or(false);
        let source = if matches!(session.status, Status::Done | Status::Canceled) {
            SessionDiffTaskSource::Archived {
                repositories: self.services.db().clone(),
            }
        } else if session.status == Status::Draft && !has_worktree {
            SessionDiffTaskSource::Empty
        } else {
            SessionDiffTaskSource::Worktree {
                archived_fallback: (session.status == Status::Merging)
                    .then(|| self.services.db().clone()),
                base_branch: session.base_branch.clone(),
                git_client: self.services.git_client(),
            }
        };

        (session.folder.clone(), source)
    }

    /// Spawns one diff task from captured session metadata and registers its
    /// foreground continuation.
    fn spawn_session_diff_request_for_target(
        &mut self,
        session_id: &SessionId,
        purpose: SessionDiffPurpose,
        folder: PathBuf,
        source: SessionDiffTaskSource,
    ) -> u64 {
        let input = SessionDiffTaskInput {
            app_event_tx: self.services.event_sender(),
            folder,
            session_id: session_id.clone(),
            source,
        };
        let request_id = TaskService::spawn_session_diff_task(input);
        self.pending_session_diff_requests.insert(
            request_id,
            PendingSessionDiffRequest {
                purpose,
                session_id: session_id.clone(),
            },
        );

        request_id
    }

    /// Starts one deduplicated review-preparation request and shows loading
    /// state only when there is no prior review output to retain.
    ///
    /// The review agent is the active project's, so only a session on the
    /// active page starts one here.
    fn start_review_diff_load(&mut self, session_id: &SessionId, is_manual: bool) -> bool {
        let Some(session) = self.sessions.active_page_session(session_id) else {
            return false;
        };
        let review_agent = review::session_review_agent(
            review::normalize_review_agent(self.review_agent()),
            session.agent,
            &self.services.available_agent_kinds(),
        );
        let (folder, source) = self.session_diff_task_target(session);

        self.start_review_diff_load_for_target(
            session_id,
            is_manual,
            FocusedReviewTarget {
                folder: folder.clone(),
                review_agent,
            },
            folder,
            source,
        )
    }

    /// Starts one review-preparation request from stable target metadata.
    fn start_review_diff_load_for_target(
        &mut self,
        session_id: &SessionId,
        is_manual: bool,
        target: FocusedReviewTarget,
        folder: PathBuf,
        source: SessionDiffTaskSource,
    ) -> bool {
        if self.pending_session_diff_requests.values().any(|request| {
            request.session_id == *session_id
                && matches!(&request.purpose, SessionDiffPurpose::Review { .. })
        }) {
            return false;
        }

        let cached_diff_hash = self
            .review_cache
            .get(session_id)
            .and_then(ReviewCacheEntry::diff_hash);
        let review_agent = target.review_agent;
        self.spawn_session_diff_request_for_target(
            session_id,
            SessionDiffPurpose::Review {
                cached_diff_hash,
                is_manual,
                target,
            },
            folder,
            source,
        );

        if is_manual && cached_diff_hash.is_none() {
            self.review_cache.insert(
                session_id.clone(),
                ReviewCacheEntry::Loading {
                    request_id: uuid::Uuid::new_v4(),
                    progress: None,
                    diff_hash: review::diff_content_hash(""),
                    review_agent,
                },
            );
            review::mark_session_agent_review(self.sessions.state_mut(), session_id);
            if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
                session.transient_messages.upsert(TransientMessage {
                    anchor: TransientMessageAnchor::Tail,
                    body: TransientMessageBody::Loading(review::review_loading_message(
                        review_agent,
                    )),
                    lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
                    slot: TransientMessageSlot::Review,
                    turn_position: session.latest_user_prompt_position(),
                });
            }
        }

        true
    }

    /// Replaces a matching diff-loading page with the loaded workspace,
    /// including an empty one, or restores its source page after a load
    /// failure.
    fn apply_open_diff_update(&mut self, update: SessionDiffUpdate) {
        let mode = std::mem::replace(&mut self.mode, AppMode::List);
        let AppMode::DiffLoading {
            fallback_view_scroll_offset,
            request_id,
            restore,
            session_id,
            sidebar_focus,
        } = mode
        else {
            self.mode = mode;

            return;
        };
        if request_id != update.request_id || session_id != update.session_id {
            self.mode = AppMode::DiffLoading {
                fallback_view_scroll_offset,
                request_id,
                restore,
                session_id,
                sidebar_focus,
            };

            return;
        }

        let diff = match update.result {
            Ok(diff) => diff,
            Err(error) => {
                self.mode = restore.map_or_else(
                    || AppMode::View {
                        scroll_offset: fallback_view_scroll_offset,
                        session_id: session_id.clone(),
                    },
                    |restore| restore.into_mode(),
                );
                self.sessions.append_workflow_notice(
                    &session_id,
                    crate::domain::transcript_notice::TranscriptNotice::Error
                        .format_line(format!("Unable to load diff: {error}")),
                );

                return;
            }
        };
        let mut review_comments = self.start_session_review_comment_load(&session_id);
        if let Some(review_comments) = &mut review_comments {
            review_comments.sidebar_focus = sidebar_focus;
        }
        self.mode = AppMode::Diff {
            diff,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: self
                .diff_comment_progress
                .remove(&session_id)
                .unwrap_or_default(),
            preview: DiffPreview::default(),
            review_comments,
            restore,
            scroll_cache: None,
            scroll_offset: 0,
            selected_diff_line_index: 0,
            session_id,
        };
    }

    /// Queues a reloaded diff for the open page only when it differs from the
    /// visible diff. A failed re-read keeps the visible diff unchanged.
    fn apply_refreshed_diff_update(&mut self, update: SessionDiffUpdate) {
        let AppMode::Diff {
            diff, session_id, ..
        } = &self.mode
        else {
            return;
        };
        if *session_id != update.session_id {
            return;
        }
        let Ok(refreshed_diff) = update.result else {
            return;
        };
        if *diff == refreshed_diff {
            self.open_diff_refresh.refreshed = None;

            return;
        }

        self.queue_refreshed_session_diff(update.session_id, refreshed_diff);
    }

    /// Validates and starts focused review generation from one background
    /// diff result, preserving cache generations when the result is stale.
    async fn apply_review_diff_update(
        &mut self,
        update: SessionDiffUpdate,
        cached_diff_hash: Option<u64>,
        is_manual: bool,
        target: FocusedReviewTarget,
    ) {
        let session_id = update.session_id;
        let status = if let Some(session) = self.sessions.session_for_id(&session_id) {
            Some(session.status)
        } else {
            self.services
                .db()
                .sessions()
                .load_session(session_id.as_str())
                .await
                .ok()
                .flatten()
                .and_then(|row| row.status.parse::<Status>().ok())
        };
        if !status.is_some_and(Status::allows_review_actions) {
            if cached_diff_hash.is_none() {
                self.clear_review_output(&session_id);
            }

            return;
        }
        self.deferred_auto_review_session_ids.remove(&session_id);
        let diff = match update.result {
            Ok(diff) if !diff.starts_with("Failed to run git diff:") => diff,
            Ok(error) | Err(error) => {
                let persistence = review::fail_review_preparation(
                    &mut self.review_cache,
                    self.sessions.state_mut(),
                    &session_id,
                    error,
                );
                self.persist_focused_review_updates(vec![persistence]).await;
                review::restore_session_review_status(self.sessions.state_mut(), &session_id);

                return;
            }
        };
        let diff_hash = review::diff_content_hash(&diff);
        let should_start = match self
            .prepare_review_diff(
                &session_id,
                diff_hash,
                !diff.trim().is_empty() && cached_diff_hash != Some(diff_hash),
                is_manual,
            )
            .await
        {
            Ok(should_start) => should_start,
            Err(error) => {
                warn!(%session_id, %error, "Failed to claim focused review");
                let persistence = review::fail_review_preparation(
                    &mut self.review_cache,
                    self.sessions.state_mut(),
                    &session_id,
                    format!("Failed to claim focused review: {error}"),
                );
                self.persist_focused_review_updates(vec![persistence]).await;
                review::restore_session_review_status(self.sessions.state_mut(), &session_id);

                return;
            }
        };
        if should_start {
            self.start_review_assist(
                &session_id,
                &target.folder,
                diff_hash,
                &diff,
                target.review_agent,
            )
            .await;

            return;
        }
        if is_manual && diff.trim().is_empty() {
            let _ = self
                .services
                .db()
                .sessions()
                .update_session_focused_review(&session_id, None, None, None)
                .await;
            self.set_review_ready_output(
                &session_id,
                diff_hash,
                review::REVIEW_NO_DIFF_MESSAGE.to_string(),
            );
        } else if !is_manual && cached_diff_hash.is_none() {
            let _ = self
                .services
                .db()
                .sessions()
                .update_session_focused_review(&session_id, None, None, None)
                .await;
            self.clear_review_output(&session_id);
        }
        review::restore_session_review_status(self.sessions.state_mut(), &session_id);
    }

    /// Recovers the baseline and commits it together with any pending-review
    /// claim before the caller starts generation. Failed writes leave the
    /// in-memory baseline unchanged so a later attempt can retry.
    async fn prepare_review_diff(
        &mut self,
        session_id: &SessionId,
        diff_hash: u64,
        review_allowed: bool,
        is_manual: bool,
    ) -> Result<bool, DbError> {
        let persisted = self
            .services
            .db()
            .sessions()
            .load_session_review_diff_hash(session_id)
            .await?;
        let previous = self
            .review_diff_hashes
            .get(session_id)
            .copied()
            .or_else(|| persisted.and_then(|hash| hash.parse().ok()));
        let should_start = review_allowed && (is_manual || previous != Some(diff_hash));
        self.services
            .db()
            .sessions()
            .update_session_review_diff_hash(session_id, &diff_hash.to_string(), should_start)
            .await?;
        self.review_diff_hashes
            .insert(session_id.clone(), diff_hash);

        Ok(should_start)
    }
}

/// Splits a unified diff into its changed files, with each file's status,
/// line counts, and patch.
fn api_diff_files(diff: &str) -> Vec<SessionDiffFile> {
    DiffFile::parse(diff)
        .into_iter()
        .map(|file| {
            let header = file
                .text
                .split_once("\n@@")
                .map_or(file.text, |(header, _)| header);
            let has_header_line =
                |prefix: &str| header.lines().any(|line| line.starts_with(prefix));
            let status = if has_header_line("new file mode") {
                SessionDiffFileStatus::Added
            } else if has_header_line("deleted file mode") {
                SessionDiffFileStatus::Deleted
            } else if file.old_path != file.new_path {
                SessionDiffFileStatus::Renamed
            } else {
                SessionDiffFileStatus::Modified
            };
            let (added_lines, deleted_lines) = SessionStats::line_change_counts(file.text);

            SessionDiffFile {
                added_lines,
                deleted_lines,
                old_path: (status == SessionDiffFileStatus::Renamed).then_some(file.old_path),
                patch: file.text.to_string(),
                path: file.new_path,
                status,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "session_diff_test.rs"]
mod tests;
