//! Background full-diff request tracking and reducer-owned completion handling.

use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::path::PathBuf;

use tracing::warn;

use crate::app::review::{self, FocusedReviewPersistence, ReviewAgent, ReviewCacheEntry};
use crate::app::task::{SessionDiffTaskInput, SessionDiffTaskSource, TaskService};
use crate::app::{App, session};
use crate::domain::review::FocusedReviewStatus;
use crate::domain::session::{Session, SessionId, SessionRole, Status};
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
    Open {
        allow_empty: bool,
    },
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
        allow_empty: bool,
    ) -> bool {
        let fallback_view_scroll_offset = match &self.mode {
            AppMode::View {
                scroll_offset,
                session_id: viewed_session_id,
            } if viewed_session_id == session_id => *scroll_offset,
            _ => None,
        };
        let Some(request_id) =
            self.spawn_session_diff_request(session_id, SessionDiffPurpose::Open { allow_empty })
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
        self.mode = restore.map_or_else(
            || AppMode::View {
                scroll_offset: fallback_view_scroll_offset,
                session_id,
            },
            |restore| restore.into_mode(),
        );
    }

    /// Discards every diff continuation and deferred automatic-review trigger
    /// owned by a deleted session so detached task completions remain stale.
    pub(crate) fn discard_deleted_session_diff_state(&mut self, session_id: &SessionId) {
        self.auto_address_review_iterations.remove(session_id);
        self.review_diff_hashes.remove(session_id);
        self.diff_comment_progress.remove(session_id);
        self.pending_session_diff_requests
            .retain(|_, request| request.session_id != *session_id);
        self.deferred_auto_review_session_ids.remove(session_id);
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
        self.start_review_diff_load(session_id, true)
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

    /// Starts automatic review diff loads for eligible touched sessions.
    ///
    /// Requests are deduplicated per session, and existing generated output
    /// remains visible while its current diff hash is checked in the
    /// background.
    pub(super) fn start_auto_review_diff_loads(&mut self, session_ids: &HashSet<SessionId>) {
        for session_id in session_ids {
            let Some(session) = self.sessions.session_for_id(session_id) else {
                continue;
            };
            let current_status = session.status;
            let session_role = session.role;

            if current_status == Status::InProgress {
                self.discard_pending_review_action_diff_loads(session_id);
                self.clear_review_output(session_id);

                continue;
            }
            if session_role == SessionRole::Orchestrator
                || !matches!(current_status, Status::Review | Status::AgentReview)
                || matches!(
                    self.review_cache.get(session_id),
                    Some(ReviewCacheEntry::Loading { .. } | ReviewCacheEntry::Suppressed)
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
        let role = row
            .role
            .as_deref()
            .and_then(|value| value.parse::<SessionRole>().ok())
            .unwrap_or_default();
        if role == SessionRole::Orchestrator
            || !matches!(status, Some(Status::Review | Status::AgentReview))
        {
            return false;
        }
        let Some(project_id) = row.project_id else {
            return false;
        };
        let review_agent =
            crate::app::setting::load_default_review_agent_setting(&self.services, project_id)
                .await;
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
            SessionDiffPurpose::Open { allow_empty } => {
                self.apply_open_diff_update(update, allow_empty);
            }
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
                    ReviewCacheEntry::Ready { diff_hash, text }
                        if *diff_hash == cached_diff_hash =>
                    {
                        Some(text.clone())
                    }
                    _ => None,
                })
        else {
            return;
        };
        let review_generation_is_current = self
            .sessions
            .session_for_id(session_id)
            .is_some_and(|session| session.status == Status::Review);
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
            let auto_address_mode_is_current = self
                .sessions
                .session_for_id(session_id)
                .is_some_and(|session| {
                    session.permission_mode
                        == crate::domain::permission::PermissionMode::AutoEditAddressComments
                });
            let completed_iterations = self
                .auto_address_review_iterations
                .get(session_id)
                .copied()
                .unwrap_or(0);
            if !auto_address_mode_is_current
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
                self.set_review_ready_output(
                    session_id,
                    cached_diff_hash,
                    current_review_text.clone(),
                );
                self.persist_focused_review_updates(vec![FocusedReviewPersistence {
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

    /// Discards detached diff tasks whose continuations belong to an obsolete
    /// focused-review turn. Their eventual events are ignored as stale.
    fn discard_pending_review_action_diff_loads(&mut self, session_id: &SessionId) {
        self.pending_session_diff_requests.retain(|_, request| {
            request.session_id != *session_id || !request.purpose.is_review_action()
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

    /// Captures one loaded session's folder and archive-or-worktree diff
    /// source.
    fn session_diff_task_target(&self, session: &Session) -> (PathBuf, SessionDiffTaskSource) {
        let source = if session.is_managed()
            && (session.status == Status::Done
                || (session.role == SessionRole::OrchestrationResearcher
                    && session.status == Status::Canceled))
        {
            SessionDiffTaskSource::Archived {
                repositories: self.services.db().clone(),
            }
        } else {
            SessionDiffTaskSource::Worktree {
                archived_fallback: (session.is_managed() && session.status == Status::Merging)
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
    fn start_review_diff_load(&mut self, session_id: &SessionId, is_manual: bool) -> bool {
        let review_agent = review::normalize_review_agent(self.review_agent());
        let Some(session) = self.sessions.session_for_id(session_id) else {
            return false;
        };
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

    /// Replaces a matching diff-loading page with the loaded workspace or its
    /// preserved source page when unchanged sessions do not expose `d`.
    fn apply_open_diff_update(&mut self, update: SessionDiffUpdate, allow_empty: bool) {
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
        if diff.trim().is_empty() && !allow_empty {
            self.mode = restore.map_or_else(
                || AppMode::View {
                    scroll_offset: fallback_view_scroll_offset,
                    session_id,
                },
                |restore| restore.into_mode(),
            );

            return;
        }

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
        if !matches!(status, Some(Status::Review | Status::AgentReview)) {
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

#[cfg(test)]
#[path = "session_diff_test.rs"]
mod tests;
