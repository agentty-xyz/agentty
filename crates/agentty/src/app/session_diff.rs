//! Background full-diff request tracking and reducer-owned completion handling.

use std::collections::HashSet;
use std::collections::hash_map::Entry;

use crate::app::App;
use crate::app::review::{self, FocusedReviewPersistence, ReviewCacheEntry};
use crate::app::task::{SessionDiffTaskInput, SessionDiffTaskSource, TaskService};
use crate::domain::review::FocusedReviewStatus;
use crate::domain::session::{SessionId, SessionRole, Status};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::presentation::app_mode::{AppMode, DiffPreview, DiffRestoreTarget, DiffSidebarFocus};

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

/// Action resumed after one full diff finishes loading.
enum SessionDiffPurpose {
    ApplyFocusedReview {
        cached_diff_hash: u64,
        suggestions: String,
    },
    Open {
        allow_empty: bool,
    },
    Review {
        cached_diff_hash: Option<u64>,
        is_manual: bool,
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
        if self.pending_session_diff_requests.values().any(|request| {
            request.session_id == *session_id && request.purpose.is_apply_focused_review()
        }) {
            return false;
        }

        self.spawn_session_diff_request(
            session_id,
            SessionDiffPurpose::ApplyFocusedReview {
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
                cached_diff_hash,
                suggestions,
            } => {
                self.apply_focused_review_diff_update(
                    &update.session_id,
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
            } => {
                self.apply_review_diff_update(update, cached_diff_hash, is_manual)
                    .await;
            }
        }
    }

    /// Completes `/apply` only when the background diff still matches the
    /// focused-review generation selected by the user.
    async fn apply_focused_review_diff_update(
        &mut self,
        session_id: &SessionId,
        cached_diff_hash: u64,
        suggestions: &str,
        result: Result<String, String>,
    ) {
        let review_generation_is_current = self
            .sessions
            .session_for_id(session_id)
            .is_some_and(|session| session.status == Status::Review)
            && matches!(
                self.review_cache.get(session_id),
                Some(ReviewCacheEntry::Ready { diff_hash, .. })
                    if *diff_hash == cached_diff_hash
            );
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

        self.reply(
            session_id,
            crate::app::prompt_intent::build_apply_review_prompt(suggestions),
        )
        .await;
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
                base_branch: session.base_branch.clone(),
                git_client: self.services.git_client(),
            }
        };
        let input = SessionDiffTaskInput {
            app_event_tx: self.services.event_sender(),
            folder: session.folder.clone(),
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

        Some(request_id)
    }

    /// Starts one deduplicated review-preparation request and shows loading
    /// state only when there is no prior review output to retain.
    fn start_review_diff_load(&mut self, session_id: &SessionId, is_manual: bool) -> bool {
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
        if self
            .spawn_session_diff_request(
                session_id,
                SessionDiffPurpose::Review {
                    cached_diff_hash,
                    is_manual,
                },
            )
            .is_none()
        {
            return false;
        }

        if is_manual && cached_diff_hash.is_none() {
            self.review_cache.insert(
                session_id.clone(),
                ReviewCacheEntry::Loading {
                    diff_hash: review::diff_content_hash(""),
                },
            );
            review::mark_session_agent_review(self.sessions.state_mut(), session_id);
            if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
                session.transient_messages.upsert(TransientMessage {
                    anchor: TransientMessageAnchor::Tail,
                    body: TransientMessageBody::Loading(review::review_loading_message(
                        self.settings.default_review_selection.model(),
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

        let diff = update.result.unwrap_or_else(|error| error);
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
            preview: DiffPreview::default(),
            review_comments,
            restore,
            scroll_cache: None,
            scroll_offset: 0,
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
    ) {
        let session_id = update.session_id;
        let Some(session) = self.sessions.session_for_id(&session_id) else {
            if cached_diff_hash.is_none() {
                self.review_cache.remove(&session_id);
            }

            return;
        };
        if !matches!(session.status, Status::Review | Status::AgentReview) {
            if cached_diff_hash.is_none() {
                self.clear_review_output(&session_id);
            }

            return;
        }
        let session_folder = session.folder.clone();
        let diff = match update.result {
            Ok(diff) if !diff.starts_with("Failed to run git diff:") => diff,
            Ok(error) | Err(error) => {
                let persistence = review::fail_review_preparation(
                    &mut self.review_cache,
                    self.sessions.state_mut(),
                    &session_id,
                    error,
                    self.settings.default_review_selection.model(),
                );
                self.persist_focused_review_updates(vec![persistence]).await;
                review::restore_session_review_status(self.sessions.state_mut(), &session_id);

                return;
            }
        };
        let diff_hash = review::diff_content_hash(&diff);
        if diff.trim().is_empty() {
            if is_manual {
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
            } else if cached_diff_hash.is_none() {
                self.clear_review_output(&session_id);
            }
            review::restore_session_review_status(self.sessions.state_mut(), &session_id);

            return;
        }
        if cached_diff_hash == Some(diff_hash) {
            review::restore_session_review_status(self.sessions.state_mut(), &session_id);

            return;
        }

        self.start_review_assist(&session_id, &session_folder, diff_hash, &diff);
        self.persist_focused_review_updates(vec![FocusedReviewPersistence {
            diff_hash: Some(diff_hash),
            session_id,
            status: FocusedReviewStatus::Pending,
            text: None,
        }])
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::{
        ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
    };

    /// Builds one Git-backed review session for diff-request state tests.
    async fn review_app() -> (App, tempfile::TempDir, SessionId) {
        let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
        let session_id = SessionId::from(
            app.create_session()
                .await
                .expect("session should be created"),
        );
        app.sessions.sessions_mut()[0].status = Status::Review;

        (app, base_dir, session_id)
    }

    /// Returns the active loading request generation.
    fn loading_request_id(app: &App) -> Option<u64> {
        match app.mode {
            AppMode::DiffLoading { request_id, .. } => Some(request_id),
            _ => None,
        }
    }

    /// Attaches one open review request to the created session.
    fn attach_review_request(app: &mut App) {
        app.sessions.sessions_mut()[0].review_request = Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session-diff".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Session diff".to_string(),
                web_url: "https://example.test/pull/42".to_string(),
            },
        });
    }

    #[tokio::test]
    async fn cancel_diff_view_load_restores_view_and_discards_completion() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        app.mode = AppMode::View {
            scroll_offset: Some(7),
            session_id: session_id.clone(),
        };
        assert_eq!(loading_request_id(&app), None);
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

        // Act
        app.cancel_diff_view_load();
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("stale diff".to_string()),
            session_id: session_id.clone(),
        })
        .await;
        app.cancel_diff_view_load();

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref viewed_session_id,
                scroll_offset: Some(7),
            } if viewed_session_id == &session_id
        ));
    }

    #[tokio::test]
    async fn stale_open_diff_completion_keeps_newer_loading_generation() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");
        let newer_request_id = request_id.saturating_add(1);
        app.mode = AppMode::DiffLoading {
            fallback_view_scroll_offset: None,
            request_id: newer_request_id,
            restore: None,
            session_id: session_id.clone(),
            sidebar_focus: DiffSidebarFocus::Comments,
        };

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("stale diff".to_string()),
            session_id,
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::DiffLoading {
                request_id,
                sidebar_focus: DiffSidebarFocus::Comments,
                ..
            } if request_id == newer_request_id
        ));
    }

    #[tokio::test]
    async fn open_diff_completion_after_mode_change_is_discarded() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");
        app.mode = AppMode::List;

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("stale diff".to_string()),
            session_id,
        })
        .await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert!(app.pending_session_diff_requests.is_empty());
    }

    #[tokio::test]
    async fn open_diff_completion_preserves_comment_focus() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        attach_review_request(&mut app);
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Comments, true,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("diff --git a/file b/file\n+change".to_string()),
            session_id,
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(crate::presentation::app_mode::DiffReviewComments {
                    sidebar_focus: DiffSidebarFocus::Comments,
                    ..
                }),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn mismatched_session_diff_completion_is_ignored() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
        let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("wrong session".to_string()),
            session_id: "other-session".into(),
        })
        .await;

        // Assert
        assert!(matches!(app.mode, AppMode::DiffLoading { .. }));
        assert!(app.pending_session_diff_requests.contains_key(&request_id));
    }

    #[tokio::test]
    async fn missing_session_cannot_start_diff_requests() {
        // Arrange
        let (mut app, _base_dir, _session_id) = review_app().await;
        let missing_session_id = SessionId::from("missing-session");

        // Act
        let diff_started =
            app.start_diff_view_load(&missing_session_id, None, DiffSidebarFocus::Files, false);
        let apply_started =
            app.start_apply_review_diff_load(&missing_session_id, 1, "suggestion".to_string());
        let review_started = app.start_manual_review_diff_load(&missing_session_id);

        // Assert
        assert!(!diff_started);
        assert!(!apply_started);
        assert!(!review_started);
    }

    #[tokio::test]
    async fn apply_review_diff_request_is_deduplicated_per_session() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;

        // Act
        let first_started =
            app.start_apply_review_diff_load(&session_id, 1, "first suggestion".to_string());
        let second_started =
            app.start_apply_review_diff_load(&session_id, 1, "duplicate suggestion".to_string());

        // Assert
        assert!(first_started);
        assert!(!second_started);
        assert_eq!(app.pending_session_diff_requests.len(), 1);
        assert!(app.pending_session_diff_requests.values().any(|request| {
            request.session_id == session_id
                && matches!(
                    &request.purpose,
                    SessionDiffPurpose::ApplyFocusedReview { .. }
                )
        }));
    }

    #[tokio::test]
    async fn clearing_review_output_discards_pending_apply_request() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash: 1,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_apply_review_diff_load(&session_id, 1, "- Fix the issue.".to_string(),));

        // Act
        app.clear_review_output(&session_id);

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn apply_completion_ignores_replaced_review_generation() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
        let current_diff = String::new();
        let diff_hash = review::diff_content_hash(&current_diff);
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_apply_review_diff_load(
            &session_id,
            diff_hash,
            "- Fix the issue.".to_string(),
        ));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("apply diff request should be pending");
        app.review_cache
            .insert(session_id.clone(), ReviewCacheEntry::Loading { diff_hash });

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok(current_diff),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Loading { diff_hash: cached_hash })
                if *cached_hash == diff_hash
        ));
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    }

    #[tokio::test]
    async fn apply_completion_ignores_session_that_left_review() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
        let current_diff = String::new();
        let diff_hash = review::diff_content_hash(&current_diff);
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_apply_review_diff_load(
            &session_id,
            diff_hash,
            "- Fix the issue.".to_string(),
        ));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("apply diff request should be pending");
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok(current_diff),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Ready {
                diff_hash: cached_hash,
                ..
            }) if *cached_hash == diff_hash
        ));
        assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
    }

    #[tokio::test]
    async fn review_diff_request_is_deduplicated_and_cleared_after_status_change() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;

        // Act
        let first_started = app.start_manual_review_diff_load(&session_id);
        let second_started = app.start_manual_review_diff_load(&session_id);
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("review diff request should be pending");
        app.sessions.sessions_mut()[0].status = Status::InProgress;
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("diff".to_string()),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(first_started);
        assert!(!second_started);
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn superseding_review_turn_discards_review_action_requests() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_manual_review_diff_load(&session_id));
        assert!(app.start_apply_review_diff_load(&session_id, 1, "suggestion".to_string()));
        let completed_sessions = HashSet::from([session_id.clone()]);

        // Act
        app.supersede_review_diff_loads(&completed_sessions);

        // Assert
        assert!(app.pending_session_diff_requests.is_empty());
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn review_completion_for_removed_session_clears_loading_cache() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_manual_review_diff_load(&session_id));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("review diff request should be pending");
        app.sessions.remove_session_at(0);

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok("diff".to_string()),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert!(!app.review_cache.contains_key(&session_id));
    }

    #[tokio::test]
    async fn review_diff_failure_restores_review_status() {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app().await;
        assert!(app.start_manual_review_diff_load(&session_id));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("review diff request should be pending");

        // Act
        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Err("Failed to run git diff: unavailable".to_string()),
            session_id: session_id.clone(),
        })
        .await;

        // Assert
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
        assert!(matches!(
            app.review_cache.get(&session_id),
            Some(ReviewCacheEntry::Failed { error, .. }) if error.contains("unavailable")
        ));
    }
}
