//! Focused review-cache and review-assist orchestration helpers.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::task;
use crate::app::service::AppServices;
use crate::app::session_state::SessionState;
use crate::domain::agent::{AgentModel, AgentSelection};
use crate::domain::session::{SessionId, Status};
use crate::domain::session_message::{SessionMessageKind, SessionMessageState, SessionTranscript};
use crate::infra::db::{SessionFocusedReviewRow, SessionTimelineMessage};

/// Cached focused review state for a session.
#[derive(Debug)]
pub(crate) enum ReviewCacheEntry {
    /// Review generation is in progress.
    Loading {
        /// Hash of the diff text that triggered this review generation.
        diff_hash: u64,
        /// Turn that owns the in-flight focused-review timeline entry.
        turn_id: i64,
    },
    /// Review text was successfully generated.
    Ready {
        /// Hash of the diff text that was reviewed.
        diff_hash: u64,
        /// Generated review text.
        text: String,
    },
    /// Review generation failed with an error description.
    Failed {
        /// Hash of the diff text that triggered the failed review.
        diff_hash: u64,
        /// Human-readable error description.
        error: String,
    },
    /// Automatic focused review is intentionally suppressed for this diff.
    ///
    /// Manual focused review can still replace this entry with `Loading`.
    Suppressed {
        /// Hash of the diff text that should not auto-start review assist.
        diff_hash: u64,
    },
}

impl ReviewCacheEntry {
    /// Returns the diff content hash stored in any variant.
    pub(crate) fn diff_hash(&self) -> u64 {
        match self {
            Self::Loading { diff_hash, .. }
            | Self::Ready { diff_hash, .. }
            | Self::Failed { diff_hash, .. }
            | Self::Suppressed { diff_hash } => *diff_hash,
        }
    }

    /// Builds one cache entry from a completed focused-review result.
    pub(crate) fn from_result(diff_hash: u64, result: &Result<String, String>) -> Self {
        match result {
            Ok(review_text) => Self::Ready {
                diff_hash,
                text: review_text.clone(),
            },
            Err(error) => Self::Failed {
                diff_hash,
                error: error.clone(),
            },
        }
    }
}

/// Aggregated review assist output keyed by session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewUpdate {
    /// Hash of the diff that triggered this review, carried from the task.
    pub(crate) diff_hash: u64,
    /// Completed review assist result for the matching session.
    pub(crate) result: Result<String, String>,
    /// Turn that owned the review task when it started.
    pub(crate) turn_id: i64,
}

/// Persists focused-review timeline states and keeps failures visible.
pub(crate) struct FocusedReviewTimeline<'a> {
    /// Focused-review cache kept consistent with persistence outcomes.
    pub(crate) review_cache: &'a mut HashMap<SessionId, ReviewCacheEntry>,
    /// Shared application services used for persistence and redraw events.
    pub(crate) services: &'a AppServices,
    /// Shared live transcript updated after each write.
    pub(crate) transcript: &'a Arc<Mutex<SessionTranscript>>,
}

impl FocusedReviewTimeline<'_> {
    /// Persists one focused-review state, replacing storage failures with
    /// visible retry guidance in the live transcript.
    pub(crate) async fn persist(
        &mut self,
        session_id: &str,
        diff_hash: u64,
        turn_id: i64,
        content: &str,
        state: SessionMessageState,
    ) -> Result<(), super::session::SessionError> {
        let entry_key = focused_review_entry_key(turn_id, diff_hash);
        let app_event_tx = self.services.event_sender();
        let session_update_versions = self.services.session_update_versions();
        let result = super::session::SessionTaskService::upsert_timeline_message(
            self.transcript,
            self.services.db(),
            &app_event_tx,
            &session_update_versions,
            session_id,
            SessionTimelineMessage {
                content,
                entry_key: &entry_key,
                kind: SessionMessageKind::FocusedReview,
                state,
                turn_id,
            },
        )
        .await;
        if let Err(error) = result {
            self.record_failure(
                session_id,
                diff_hash,
                turn_id,
                &entry_key,
                &error.to_string(),
            )
            .await;

            return Err(error);
        }

        Ok(())
    }

    async fn record_failure(
        &mut self,
        session_id: &str,
        diff_hash: u64,
        turn_id: i64,
        entry_key: &str,
        storage_error: &str,
    ) {
        tracing::warn!(
            session_id,
            entry_key,
            error = storage_error,
            "failed to persist focused-review timeline entry"
        );
        self.review_cache.insert(
            SessionId::from(session_id),
            ReviewCacheEntry::Failed {
                diff_hash,
                error: REVIEW_PERSISTENCE_ERROR.to_string(),
            },
        );

        let content = format!("Review assist unavailable: {REVIEW_PERSISTENCE_ERROR}");
        let app_event_tx = self.services.event_sender();
        let session_update_versions = self.services.session_update_versions();
        let update = SessionTimelineMessage {
            content: &content,
            entry_key,
            kind: SessionMessageKind::FocusedReview,
            state: SessionMessageState::Failed,
            turn_id,
        };
        if super::session::SessionTaskService::upsert_timeline_message(
            self.transcript,
            self.services.db(),
            &app_event_tx,
            &session_update_versions,
            session_id,
            update,
        )
        .await
        .is_err()
        {
            super::session::SessionTaskService::upsert_live_timeline_message(
                self.transcript,
                &app_event_tx,
                &session_update_versions,
                session_id,
                update,
            );
        }
    }
}

/// Prefix for the focused-review loading status while assist output is being
/// prepared.
const REVIEW_LOADING_MESSAGE_PREFIX: &str = "Reviewing changes with";

/// Retry guidance shown when focused-review timeline persistence fails.
const REVIEW_PERSISTENCE_ERROR: &str = "Failed to save focused review. Press f to retry.";

/// Computes a deterministic `FNV-1a` hash of diff text for focused-review
/// cache invalidation.
pub(crate) fn diff_content_hash(diff: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    diff.as_bytes().iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Builds the stable identity for one focused review owned by a session turn.
pub(crate) fn focused_review_entry_key(turn_id: i64, diff_hash: u64) -> String {
    format!("focused_review:{turn_id}:{diff_hash}")
}

/// Returns whether one status line represents an in-flight focused review.
pub(crate) fn is_review_loading_status_message(status_message: &str) -> bool {
    status_message.starts_with(REVIEW_LOADING_MESSAGE_PREFIX)
}

/// Formats the focused-review loading status with the active model name.
pub(crate) fn review_loading_message(review_model: AgentModel) -> String {
    format!("{REVIEW_LOADING_MESSAGE_PREFIX} {}", review_model.as_str())
}

/// Returns the focused-review render state that should be restored for one
/// session when reopening session view.
pub(crate) fn review_view_state<'a>(
    review_cache: &'a HashMap<SessionId, ReviewCacheEntry>,
    session_id: &str,
    review_model: AgentModel,
) -> (Option<String>, Option<&'a str>) {
    let Some(cache_entry) = review_cache.get(session_id) else {
        return (None, None);
    };

    match cache_entry {
        ReviewCacheEntry::Loading { .. } => (Some(review_loading_message(review_model)), None),
        ReviewCacheEntry::Ready { text, .. } => (None, Some(text.as_str())),
        ReviewCacheEntry::Failed { error, .. } => (
            Some(format!("Review assist unavailable: {}", error.trim())),
            None,
        ),
        ReviewCacheEntry::Suppressed { .. } => (None, None),
    }
}

/// Builds the startup focused-review cache from persisted rows.
pub(crate) fn review_cache_from_rows(
    focused_review_rows: Vec<SessionFocusedReviewRow>,
) -> HashMap<SessionId, ReviewCacheEntry> {
    focused_review_rows
        .into_iter()
        .filter_map(|row| {
            let diff_hash = row
                .entry_key
                .strip_prefix("focused_review:")?
                .rsplit(':')
                .next()?
                .parse::<u64>()
                .ok()?;

            Some((
                SessionId::from(row.session_id),
                ReviewCacheEntry::Ready {
                    diff_hash,
                    text: row.text,
                },
            ))
        })
        .collect()
}

/// Cancels an in-flight focused review for a session so a later stale
/// review-assist result is ignored.
///
/// Returns `true` when a loading review was removed, letting callers clear any
/// matching persisted review state from durable storage.
pub(crate) fn cancel_pending_review(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    session_id: &str,
) -> bool {
    let Some(ReviewCacheEntry::Loading { .. }) = review_cache.get(session_id) else {
        return false;
    };

    review_cache.remove(session_id);

    true
}

/// Marks one review-ready session as transient `AgentReview` while focused
/// review generation is running.
pub(crate) fn mark_session_agent_review(session_state: &mut SessionState, session_id: &str) {
    update_transient_review_status(
        session_state,
        session_id,
        Status::Review,
        Status::AgentReview,
    );
}

/// Applies review assist updates for all sessions in one reducer batch.
pub(crate) fn apply_review_updates(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    session_state: &mut SessionState,
    review_updates: Vec<(SessionId, ReviewUpdate)>,
) -> Vec<(SessionId, ReviewUpdate)> {
    let mut accepted_updates = Vec::new();

    for (session_id, review_update) in review_updates {
        if let Some(review_update) =
            apply_review_update(review_cache, session_state, &session_id, review_update)
        {
            accepted_updates.push((session_id, review_update));
        }
    }

    accepted_updates
}

/// Starts focused review generation for sessions that just entered review.
///
/// Uses a status-based check instead of transition detection because pending
/// `SessionUpdated` events may synchronize handle-backed status before the
/// paired review-related reducer work runs, making transition detection
/// unreliable.
///
/// Sessions returning to `InProgress`, or with an active prompt recorded
/// before their status update is reduced, clear their cached review immediately
/// so only the working loader is shown. The next completed diff then triggers
/// a fresh assist run. Sessions with a [`ReviewCacheEntry::Suppressed`] marker
/// skip diff loading entirely; stopped turns set that marker synchronously so
/// cancellation does not block on `git diff` just to prevent automatic review
/// startup.
pub(crate) async fn auto_start_reviews(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    session_ids: &HashSet<SessionId>,
    active_prompt_session_ids: &HashSet<SessionId>,
    session_state: &mut SessionState,
    services: &AppServices,
    review_selection: AgentSelection,
) {
    let app_event_tx = services.event_sender();
    let git_client = services.git_client();

    for session_id in session_ids {
        let Some(session) = session_state
            .sessions()
            .iter()
            .find(|session| session.id == *session_id)
        else {
            continue;
        };
        let current_status = session.status;

        if current_status == Status::InProgress || active_prompt_session_ids.contains(session_id) {
            review_cache.remove(session_id);

            continue;
        }

        if current_status != Status::Review {
            continue;
        }

        if matches!(
            review_cache.get(session_id),
            Some(ReviewCacheEntry::Suppressed { .. })
        ) {
            continue;
        }

        let Some(transcript) = session_state
            .handles()
            .get(session_id)
            .map(|handles| Arc::clone(&handles.transcript))
        else {
            continue;
        };

        let base_branch = session.base_branch.clone();
        let session_chat_history = session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::conversation_replay_text);
        let session_folder = session.folder.clone();

        let diff = git_client
            .diff(session_folder.clone(), base_branch)
            .await
            .unwrap_or_default();

        if diff.trim().is_empty() || diff.starts_with("Failed to run git diff:") {
            continue;
        }

        let new_hash = diff_content_hash(&diff);

        if review_cache
            .get(session_id)
            .is_some_and(|entry| entry.diff_hash() == new_hash)
        {
            continue;
        }

        let turn_id = transcript
            .lock()
            .ok()
            .map(|transcript| transcript.current_turn_id())
            .unwrap_or_default();
        review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Loading {
                diff_hash: new_hash,
                turn_id,
            },
        );
        let loading_message = review_loading_message(review_selection.model());
        let mut timeline = FocusedReviewTimeline {
            review_cache,
            services,
            transcript: &transcript,
        };
        if timeline
            .persist(
                session_id,
                new_hash,
                turn_id,
                &loading_message,
                SessionMessageState::Pending,
            )
            .await
            .is_err()
        {
            continue;
        }
        mark_session_agent_review(session_state, session_id);
        task::TaskService::spawn_review_assist_task(task::ReviewAssistTaskInput {
            app_event_tx: app_event_tx.clone(),
            diff_hash: new_hash,
            review_diff: diff,
            review_selection,
            session_chat_history,
            session_folder,
            session_id: session_id.clone(),
            turn_id,
        });
    }
}

/// Applies one review assist update to cache and session review status.
fn apply_review_update(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    session_state: &mut SessionState,
    session_id: &str,
    review_update: ReviewUpdate,
) -> Option<ReviewUpdate> {
    let cache_entry = review_cache.get(session_id)?;

    if !matches!(
        cache_entry,
        ReviewCacheEntry::Loading {
            diff_hash: cached_diff_hash,
            turn_id: cached_turn_id,
        } if *cached_diff_hash == review_update.diff_hash
            && *cached_turn_id == review_update.turn_id
    ) {
        return None;
    }

    review_cache.insert(
        SessionId::from(session_id),
        ReviewCacheEntry::from_result(review_update.diff_hash, &review_update.result),
    );
    restore_session_review_status(session_state, session_id);

    Some(review_update)
}

/// Restores one transient `AgentReview` session back to `Review` after the
/// focused-review task completes.
fn restore_session_review_status(session_state: &mut SessionState, session_id: &str) {
    update_transient_review_status(
        session_state,
        session_id,
        Status::AgentReview,
        Status::Review,
    );
}

/// Updates one session snapshot and live handle when a transient review status
/// transition still matches the expected current status.
fn update_transient_review_status(
    session_state: &mut SessionState,
    session_id: &str,
    current_status: Status,
    next_status: Status,
) {
    if let Some(session) = session_state
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        && session.status == current_status
    {
        session.status = next_status;
    }

    if let Some(handles) = session_state.handles().get(session_id)
        && let Ok(mut handle_status) = handles.status.lock()
        && *handle_status == current_status
    {
        *handle_status = next_status;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::app::session_state::SessionState;
    use crate::domain::selection::SelectionState;
    use crate::infra::clock::RealClock;

    /// Builds empty session state for review reducer tests that only need mode
    /// field updates.
    fn empty_session_state() -> SessionState {
        SessionState::new(
            HashMap::new(),
            Vec::new(),
            SelectionState::default(),
            Arc::new(RealClock),
            0,
            0,
        )
    }

    /// Builds a single loading review cache entry for one session.
    fn loading_review_cache(
        session_id: &SessionId,
        diff_hash: u64,
        turn_id: i64,
    ) -> HashMap<SessionId, ReviewCacheEntry> {
        HashMap::from([(
            session_id.clone(),
            ReviewCacheEntry::Loading { diff_hash, turn_id },
        )])
    }

    /// Builds a single successful review update for one session.
    fn successful_review_update(
        session_id: &SessionId,
        diff_hash: u64,
        review_text: &str,
        turn_id: i64,
    ) -> Vec<(SessionId, ReviewUpdate)> {
        vec![(
            session_id.clone(),
            ReviewUpdate {
                diff_hash,
                result: Ok(review_text.to_string()),
                turn_id,
            },
        )]
    }

    #[test]
    fn review_loading_message_uses_requested_model_name() {
        // Arrange
        let review_model = AgentModel::Gpt55;

        // Act
        let message = review_loading_message(review_model);

        // Assert
        assert_eq!(message, "Reviewing changes with gpt-5.5");
    }

    #[test]
    fn is_review_loading_status_message_matches_model_aware_copy() {
        // Arrange
        let status_message = review_loading_message(AgentModel::ClaudeOpus48);

        // Act
        let is_loading = is_review_loading_status_message(&status_message);

        // Assert
        assert!(is_loading);
    }

    #[test]
    fn review_view_state_uses_loading_message_for_cached_review_generation() {
        // Arrange
        let mut review_cache = HashMap::new();
        review_cache.insert(
            "session-id".into(),
            ReviewCacheEntry::Loading {
                diff_hash: 7,
                turn_id: 3,
            },
        );

        // Act
        let (review_status_message, review_text) =
            review_view_state(&review_cache, "session-id", AgentModel::ClaudeSonnet5);

        // Assert
        assert_eq!(
            review_status_message.as_deref(),
            Some("Reviewing changes with claude-sonnet-5")
        );
        assert_eq!(review_text, None);
    }

    #[test]
    fn review_view_state_hides_suppressed_auto_review() {
        // Arrange
        let mut review_cache = HashMap::new();
        review_cache.insert(
            "session-id".into(),
            ReviewCacheEntry::Suppressed { diff_hash: 7 },
        );

        // Act
        let (review_status_message, review_text) =
            review_view_state(&review_cache, "session-id", AgentModel::ClaudeSonnet5);

        // Assert
        assert_eq!(review_status_message, None);
        assert_eq!(review_text, None);
    }

    #[test]
    fn review_cache_from_rows_restores_persisted_ready_review() {
        // Arrange
        let focused_review_rows = vec![SessionFocusedReviewRow {
            entry_key: "focused_review:3:42".to_string(),
            session_id: "session-id".to_string(),
            text: "## Review\nPersisted finding.".to_string(),
        }];

        // Act
        let review_cache = review_cache_from_rows(focused_review_rows);

        // Assert
        assert!(matches!(
            review_cache.get("session-id"),
            Some(ReviewCacheEntry::Ready { diff_hash: 42, text })
                if text == "## Review\nPersisted finding."
        ));
    }

    #[test]
    fn review_cache_from_rows_accepts_legacy_diff_only_entry_key() {
        // Arrange
        let focused_review_rows = vec![SessionFocusedReviewRow {
            entry_key: "focused_review:42".to_string(),
            session_id: "legacy-session".to_string(),
            text: "## Review\nLegacy finding.".to_string(),
        }];

        // Act
        let review_cache = review_cache_from_rows(focused_review_rows);

        // Assert
        assert!(matches!(
            review_cache.get("legacy-session"),
            Some(ReviewCacheEntry::Ready { diff_hash: 42, text })
                if text == "## Review\nLegacy finding."
        ));
    }

    #[test]
    fn apply_review_updates_returns_accepted_success() {
        // Arrange
        let session_id = SessionId::from("session-persist-review");
        let diff_hash = 19;
        let review_text = "## Review\nPersist this finding.";
        let turn_id = 3;
        let mut review_cache = loading_review_cache(&session_id, diff_hash, turn_id);
        let mut session_state = empty_session_state();
        let review_updates = successful_review_update(&session_id, diff_hash, review_text, turn_id);
        let expected_updates = review_updates.clone();

        // Act
        let accepted_updates =
            apply_review_updates(&mut review_cache, &mut session_state, review_updates);

        // Assert
        assert_eq!(accepted_updates, expected_updates);
    }

    #[test]
    fn apply_review_updates_keeps_same_diff_scoped_to_owning_turn() {
        // Arrange
        let session_id = SessionId::from("session-same-diff-review");
        let diff_hash = 41;
        let first_turn_id = 1;
        let second_turn_id = 2;
        let mut review_cache = loading_review_cache(&session_id, diff_hash, first_turn_id);
        let mut session_state = empty_session_state();
        let first_updates = successful_review_update(
            &session_id,
            diff_hash,
            "## Review\nFirst turn finding.",
            first_turn_id,
        );
        let first_accepted =
            apply_review_updates(&mut review_cache, &mut session_state, first_updates);
        review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Loading {
                diff_hash,
                turn_id: second_turn_id,
            },
        );
        let second_updates = vec![
            (
                session_id.clone(),
                ReviewUpdate {
                    diff_hash,
                    result: Ok("## Review\nStale first turn result.".to_string()),
                    turn_id: first_turn_id,
                },
            ),
            (
                session_id.clone(),
                ReviewUpdate {
                    diff_hash,
                    result: Ok("## Review\nSecond turn finding.".to_string()),
                    turn_id: second_turn_id,
                },
            ),
        ];

        // Act
        let second_accepted =
            apply_review_updates(&mut review_cache, &mut session_state, second_updates);

        // Assert
        assert_eq!(
            first_accepted[0].1.turn_id, first_turn_id,
            "the first result must retain its owning turn"
        );
        assert_eq!(second_accepted.len(), 1);
        assert_eq!(second_accepted[0].0, session_id);
        assert_eq!(second_accepted[0].1.turn_id, second_turn_id);
        assert_eq!(
            second_accepted[0].1.result,
            Ok("## Review\nSecond turn finding.".to_string())
        );
    }

    #[test]
    fn apply_review_updates_returns_accepted_failure() {
        // Arrange
        let session_id = SessionId::from("session-failed-review");
        let diff_hash = 29;
        let turn_id = 4;
        let mut review_cache = loading_review_cache(&session_id, diff_hash, turn_id);
        let mut session_state = empty_session_state();
        let review_updates = vec![(
            session_id,
            ReviewUpdate {
                diff_hash,
                result: Err("provider failed".to_string()),
                turn_id,
            },
        )];
        let expected_updates = review_updates.clone();

        // Act
        let accepted_updates =
            apply_review_updates(&mut review_cache, &mut session_state, review_updates);

        // Assert
        assert_eq!(accepted_updates, expected_updates);
    }

    #[test]
    fn apply_review_updates_writes_success_to_cache() {
        // Arrange
        let session_id = SessionId::from("session-cache-review");
        let diff_hash = 11;
        let review_text = "## Review\nCache-backed finding.";
        let turn_id = 5;
        let mut review_cache = loading_review_cache(&session_id, diff_hash, turn_id);
        let mut session_state = empty_session_state();
        let review_updates = successful_review_update(&session_id, diff_hash, review_text, turn_id);

        // Act
        apply_review_updates(&mut review_cache, &mut session_state, review_updates);

        // Assert
        assert!(matches!(
            review_cache.get(session_id.as_str()),
            Some(ReviewCacheEntry::Ready { text, .. }) if text == review_text
        ));
    }

    #[test]
    fn apply_review_updates_ignores_suppressed_auto_review_entry() {
        // Arrange
        let session_id = SessionId::from("session-suppressed-review");
        let diff_hash = 23;
        let mut review_cache = HashMap::from([(
            session_id.clone(),
            ReviewCacheEntry::Suppressed { diff_hash },
        )]);
        let mut session_state = empty_session_state();
        let review_updates = successful_review_update(
            &session_id,
            diff_hash,
            "## Review\nShould not be rendered.",
            6,
        );

        // Act
        apply_review_updates(&mut review_cache, &mut session_state, review_updates);

        // Assert
        assert!(matches!(
            review_cache.get(session_id.as_str()),
            Some(ReviewCacheEntry::Suppressed { diff_hash: cached_hash })
                if *cached_hash == diff_hash
        ));
    }
}
