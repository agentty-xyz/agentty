//! Focused review-cache and review-assist orchestration helpers.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use ag_git::GitClient;
use tokio::sync::mpsc;

use super::core::AppEvent;
use super::task;
use crate::app::session_state::SessionState;
use crate::domain::agent::{AgentModel, AgentSelection, ReasoningLevel};
use crate::domain::session::{Session, SessionId, Status};
use crate::domain::session_message::SessionTranscript;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::infra::db::SessionFocusedReviewRow;

/// Cached focused review state for a session.
#[derive(Debug)]
pub(crate) enum ReviewCacheEntry {
    /// Review generation is in progress.
    Loading {
        /// Hash of the diff text that triggered this review generation.
        diff_hash: u64,
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
    /// Automatic focused review is intentionally suppressed for the current
    /// stopped turn.
    ///
    /// Manual focused review can still replace this entry with `Loading`.
    Suppressed,
}

impl ReviewCacheEntry {
    /// Returns the diff content hash stored by generated review states.
    pub(crate) fn diff_hash(&self) -> Option<u64> {
        match self {
            Self::Loading { diff_hash }
            | Self::Ready { diff_hash, .. }
            | Self::Failed { diff_hash, .. } => Some(*diff_hash),
            Self::Suppressed => None,
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
}

/// Persistable focused-review cache change produced by the reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FocusedReviewPersistence {
    /// Hash of the diff that the persisted text applies to, or `None` when
    /// clearing a stale persisted review.
    pub(crate) diff_hash: Option<u64>,
    /// Stable session identifier for the focused-review cache row.
    pub(crate) session_id: SessionId,
    /// Focused-review markdown to persist, or `None` when clearing it.
    pub(crate) text: Option<String>,
}

/// Prefix for the focused-review loading status while assist output is being
/// prepared.
const REVIEW_LOADING_MESSAGE_PREFIX: &str = "Reviewing changes with";

/// Computes a deterministic `FNV-1a` hash of diff text for focused-review
/// cache invalidation.
pub(crate) fn diff_content_hash(diff: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    diff.as_bytes().iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Formats the focused-review loading status with the active model name.
pub(crate) fn review_loading_message(review_model: AgentModel) -> String {
    format!("{REVIEW_LOADING_MESSAGE_PREFIX} {}", review_model.as_str())
}

/// Formats a focused-review failure for the session output panel.
pub(crate) fn review_failure_message(error: &str) -> String {
    format!("Review assist unavailable: {}", error.trim())
}

/// Returns focused-review markdown available to prompt actions for one
/// session.
pub(crate) fn review_view_text<'a>(
    review_cache: &'a HashMap<SessionId, ReviewCacheEntry>,
    session_id: &str,
) -> Option<&'a str> {
    let cache_entry = review_cache.get(session_id)?;

    match cache_entry {
        ReviewCacheEntry::Ready { text, .. } => Some(text.as_str()),
        ReviewCacheEntry::Loading { .. }
        | ReviewCacheEntry::Failed { .. }
        | ReviewCacheEntry::Suppressed => None,
    }
}

/// Rehydrates cached focused-review states into explicit display slots.
pub(crate) fn hydrate_review_transients(
    review_cache: &HashMap<SessionId, ReviewCacheEntry>,
    session_state: &mut SessionState,
    review_model: AgentModel,
) {
    for session in session_state.sessions_mut() {
        hydrate_session_review_transient(review_cache, session, review_model);
    }
}

/// Rehydrates one session's focused-review cache entry into its stable display
/// slot, retracting stale display state when the cache no longer owns output.
pub(crate) fn hydrate_review_transient(
    review_cache: &HashMap<SessionId, ReviewCacheEntry>,
    session_state: &mut SessionState,
    session_id: &str,
    review_model: AgentModel,
) {
    let Some(session) = session_state.session_mut_for_id(session_id) else {
        return;
    };

    hydrate_session_review_transient(review_cache, session, review_model);
}

/// Evicts inactive completed review entries while retaining in-flight work.
///
/// A `Loading` entry must survive project switches so its eventual result can
/// still be validated and persisted. Once that result arrives,
/// [`apply_review_updates()`] replaces the entry and this pruning step removes
/// it unless the session belongs to the currently loaded project.
pub(crate) fn prune_review_cache(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    session_state: &SessionState,
) {
    let active_session_ids = session_state
        .sessions()
        .iter()
        .map(|session| session.id.as_str())
        .collect::<HashSet<_>>();

    review_cache.retain(|session_id, cache_entry| {
        active_session_ids.contains(session_id.as_str())
            || matches!(cache_entry, ReviewCacheEntry::Loading { .. })
    });
}

/// Keeps a completed focused review at the position established by its
/// loading row, falling back to completed-turn placement for restored output.
pub(crate) fn focused_review_result_anchor(session: &Session) -> TransientMessageAnchor {
    session
        .transient_messages
        .get(TransientMessageSlot::Review)
        .map_or(TransientMessageAnchor::AfterCompletedTurn, |message| {
            message.anchor
        })
}

/// Synchronizes one session's focused-review display slot from the canonical
/// cache state.
fn hydrate_session_review_transient(
    review_cache: &HashMap<SessionId, ReviewCacheEntry>,
    session: &mut Session,
    review_model: AgentModel,
) {
    if !matches!(
        session.status,
        Status::Review | Status::Question | Status::AgentReview
    ) {
        session
            .transient_messages
            .retract(TransientMessageSlot::Review);

        return;
    }
    let Some(cache_entry) = review_cache.get(&session.id) else {
        session
            .transient_messages
            .retract(TransientMessageSlot::Review);

        return;
    };
    let (anchor, body) = match cache_entry {
        ReviewCacheEntry::Loading { .. } => (
            TransientMessageAnchor::Tail,
            TransientMessageBody::Loading(review_loading_message(review_model)),
        ),
        ReviewCacheEntry::Ready { text, .. } => (
            focused_review_result_anchor(session),
            TransientMessageBody::Markdown(text.clone()),
        ),
        ReviewCacheEntry::Failed { error, .. } => (
            focused_review_result_anchor(session),
            TransientMessageBody::Plain(review_failure_message(error)),
        ),
        ReviewCacheEntry::Suppressed => {
            session
                .transient_messages
                .retract(TransientMessageSlot::Review);

            return;
        }
    };

    session.transient_messages.upsert(TransientMessage {
        anchor,
        body,
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::Review,
        turn_position: session.latest_user_prompt_position(),
    });
}

/// Builds the startup focused-review cache from persisted rows.
pub(crate) fn review_cache_from_rows(
    focused_review_rows: Vec<SessionFocusedReviewRow>,
) -> HashMap<SessionId, ReviewCacheEntry> {
    focused_review_rows
        .into_iter()
        .filter_map(|row| {
            let diff_hash = row.diff_hash.parse::<u64>().ok()?;

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

/// Spawns one focused review-assist task for the provided session diff.
pub(crate) fn start_review_assist(
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    review_agent: (AgentSelection, ReasoningLevel),
    session_id: &str,
    session_folder: &Path,
    diff_hash: u64,
    review_diff: &str,
    session_chat_history: Option<&str>,
) {
    let (review_selection, reasoning_level) = review_agent;

    task::TaskService::spawn_review_assist_task(task::ReviewAssistTaskInput {
        app_event_tx,
        diff_hash,
        reasoning_level,
        review_diff: review_diff.to_string(),
        review_selection,
        session_chat_history: session_chat_history.map(str::to_string),
        session_folder: session_folder.to_path_buf(),
        session_id: SessionId::from(session_id),
    });
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
    review_updates: HashMap<SessionId, ReviewUpdate>,
) -> Vec<FocusedReviewPersistence> {
    let mut persistence_updates = Vec::new();

    for (session_id, review_update) in review_updates {
        if let Some(persistence_update) =
            apply_review_update(review_cache, session_state, &session_id, review_update)
        {
            persistence_updates.push(persistence_update);
        }
    }

    prune_review_cache(review_cache, session_state);

    persistence_updates
}

/// Starts focused review generation for sessions that just entered review.
///
/// Uses a status-based check instead of transition detection because pending
/// `SessionUpdated` events may synchronize handle-backed status before the
/// paired review-related reducer work runs, making transition detection
/// unreliable.
///
/// Sessions returning to `InProgress` clear their cached review immediately so
/// the next completed diff triggers a fresh assist run. Sessions with a
/// [`ReviewCacheEntry::Suppressed`] marker skip diff loading entirely; stopped
/// turns set that marker synchronously so cancellation does not block on `git
/// diff` just to prevent automatic review startup.
pub(crate) async fn auto_start_reviews(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    session_ids: &HashSet<SessionId>,
    session_state: &mut SessionState,
    git_client: Arc<dyn GitClient>,
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    reasoning_level: ReasoningLevel,
    review_selection: AgentSelection,
) {
    for session_id in session_ids {
        auto_start_review_for_session(
            review_cache,
            session_state,
            git_client.as_ref(),
            &app_event_tx,
            reasoning_level,
            review_selection,
            session_id,
        )
        .await;
    }
}

/// Starts focused review generation for one eligible session snapshot.
async fn auto_start_review_for_session(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    session_state: &mut SessionState,
    git_client: &dyn GitClient,
    app_event_tx: &mpsc::UnboundedSender<AppEvent>,
    reasoning_level: ReasoningLevel,
    review_selection: AgentSelection,
    session_id: &SessionId,
) {
    let Some(session) = session_state.session_for_id(session_id) else {
        return;
    };
    let current_status = session.status;

    if current_status == Status::InProgress {
        review_cache.remove(session_id);
        if let Some(session) = session_state.session_mut_for_id(session_id) {
            session
                .transient_messages
                .retract(TransientMessageSlot::Review);
        }

        return;
    }

    if current_status != Status::Review
        || matches!(
            review_cache.get(session_id),
            Some(ReviewCacheEntry::Suppressed)
        )
    {
        return;
    }

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
        return;
    }

    let new_hash = diff_content_hash(&diff);
    if review_cache
        .get(session_id)
        .is_some_and(|entry| entry.diff_hash() == Some(new_hash))
    {
        return;
    }

    review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Loading {
            diff_hash: new_hash,
        },
    );
    mark_session_agent_review(session_state, session_id);
    if let Some(session) = session_state.session_mut_for_id(session_id) {
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading(review_loading_message(review_selection.model())),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: session.latest_user_prompt_position(),
        });
    }
    start_review_assist(
        app_event_tx.clone(),
        (review_selection, reasoning_level),
        session_id,
        &session_folder,
        new_hash,
        &diff,
        session_chat_history.as_deref(),
    );
}

/// Applies one review assist update to cache and session review status.
fn apply_review_update(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    session_state: &mut SessionState,
    session_id: &str,
    review_update: ReviewUpdate,
) -> Option<FocusedReviewPersistence> {
    let ReviewUpdate { diff_hash, result } = review_update;
    let cache_entry = review_cache.get(session_id)?;

    if !matches!(cache_entry, ReviewCacheEntry::Loading { .. })
        || cache_entry.diff_hash() != Some(diff_hash)
    {
        return None;
    }

    let persistence_update = FocusedReviewPersistence {
        diff_hash: result.as_ref().ok().map(|_| diff_hash),
        session_id: SessionId::from(session_id),
        text: result.as_ref().ok().cloned(),
    };
    review_cache.insert(
        SessionId::from(session_id),
        ReviewCacheEntry::from_result(diff_hash, &result),
    );
    if let Some(session) = session_state
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        let anchor = focused_review_result_anchor(session);
        let body = match &result {
            Ok(review_text) => TransientMessageBody::Markdown(review_text.clone()),
            Err(error) => TransientMessageBody::Plain(review_failure_message(error)),
        };
        session.transient_messages.upsert(TransientMessage {
            anchor,
            body,
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: session.latest_user_prompt_position(),
        });
    }
    restore_session_review_status(session_state, session_id);

    Some(persistence_update)
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
    session_state.transition_status_if_current(session_id, current_status, next_status);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::app::session_state::SessionState;
    use crate::domain::selection::SelectionState;
    use crate::infra::clock::RealClock;
    use crate::test_support::SessionFixtureBuilder;

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
    ) -> HashMap<SessionId, ReviewCacheEntry> {
        HashMap::from([(session_id.clone(), ReviewCacheEntry::Loading { diff_hash })])
    }

    /// Builds a single successful review update for one session.
    fn successful_review_update(
        session_id: &SessionId,
        diff_hash: u64,
        review_text: &str,
    ) -> HashMap<SessionId, ReviewUpdate> {
        HashMap::from([(
            session_id.clone(),
            ReviewUpdate {
                diff_hash,
                result: Ok(review_text.to_string()),
            },
        )])
    }

    /// Builds one review-ready session with stale focused-review display text.
    fn session_state_with_stale_review(session_id: &SessionId) -> SessionState {
        let mut session = SessionFixtureBuilder::new()
            .id(session_id.as_str())
            .status(Status::Review)
            .build();
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            body: TransientMessageBody::Markdown("stale review".to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: None,
        });

        SessionState::new(
            HashMap::new(),
            vec![session],
            SelectionState::default(),
            Arc::new(RealClock),
            0,
            0,
        )
    }

    #[test]
    fn review_loading_message_uses_requested_model_name() {
        // Arrange
        let review_model = AgentModel::Gpt56Sol;

        // Act
        let message = review_loading_message(review_model);

        // Assert
        assert_eq!(message, "Reviewing changes with gpt-5.6-sol");
    }

    #[test]
    fn review_view_text_hides_cached_review_generation() {
        // Arrange
        let mut review_cache = HashMap::new();
        review_cache.insert(
            "session-id".into(),
            ReviewCacheEntry::Loading { diff_hash: 7 },
        );

        // Act
        let review_text = review_view_text(&review_cache, "session-id");

        // Assert
        assert_eq!(review_text, None);
    }

    #[test]
    fn review_view_text_hides_suppressed_auto_review() {
        // Arrange
        let mut review_cache = HashMap::new();
        review_cache.insert("session-id".into(), ReviewCacheEntry::Suppressed);

        // Act
        let review_text = review_view_text(&review_cache, "session-id");

        // Assert
        assert_eq!(review_text, None);
    }

    #[test]
    fn review_cache_from_rows_restores_persisted_ready_review() {
        // Arrange
        let focused_review_rows = vec![SessionFocusedReviewRow {
            diff_hash: "42".to_string(),
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
    fn hydrate_review_transients_retracts_terminal_session_review() {
        // Arrange
        let session_id = SessionId::from("session-id");
        let mut session = SessionFixtureBuilder::new()
            .id(session_id.as_str())
            .status(Status::Done)
            .build();
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            body: TransientMessageBody::Markdown("stale review".to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: None,
        });
        let review_cache = HashMap::from([(
            session_id,
            ReviewCacheEntry::Ready {
                diff_hash: 42,
                text: "persisted review".to_string(),
            },
        )]);
        let mut session_state = SessionState::new(
            HashMap::new(),
            vec![session],
            SelectionState::default(),
            Arc::new(RealClock),
            0,
            0,
        );

        // Act
        hydrate_review_transients(&review_cache, &mut session_state, AgentModel::Gpt56Sol);

        // Assert
        assert!(
            session_state.sessions()[0]
                .transient_messages
                .get(TransientMessageSlot::Review)
                .is_none()
        );
    }

    #[test]
    fn hydrate_review_transient_retracts_review_without_cache_entry() {
        // Arrange
        let session_id = SessionId::from("session-id");
        let mut session_state = session_state_with_stale_review(&session_id);

        // Act
        hydrate_review_transient(
            &HashMap::new(),
            &mut session_state,
            &session_id,
            AgentModel::Gpt56Sol,
        );

        // Assert
        assert!(
            session_state.sessions()[0]
                .transient_messages
                .get(TransientMessageSlot::Review)
                .is_none()
        );
    }

    #[test]
    fn hydrate_review_transient_retracts_suppressed_review() {
        // Arrange
        let session_id = SessionId::from("session-id");
        let review_cache = HashMap::from([(session_id.clone(), ReviewCacheEntry::Suppressed)]);
        let mut session_state = session_state_with_stale_review(&session_id);

        // Act
        hydrate_review_transient(
            &review_cache,
            &mut session_state,
            &session_id,
            AgentModel::Gpt56Sol,
        );

        // Assert
        assert!(
            session_state.sessions()[0]
                .transient_messages
                .get(TransientMessageSlot::Review)
                .is_none()
        );
    }

    #[test]
    fn hydrate_review_transient_restores_failed_review() {
        // Arrange
        let session_id = SessionId::from("session-id");
        let review_cache = HashMap::from([(
            session_id.clone(),
            ReviewCacheEntry::Failed {
                diff_hash: 42,
                error: "provider unavailable".to_string(),
            },
        )]);
        let mut session_state = session_state_with_stale_review(&session_id);

        // Act
        hydrate_review_transient(
            &review_cache,
            &mut session_state,
            &session_id,
            AgentModel::Gpt56Sol,
        );

        // Assert
        assert_eq!(
            session_state.sessions()[0]
                .transient_messages
                .get(TransientMessageSlot::Review)
                .map(|message| &message.body),
            Some(&TransientMessageBody::Plain(
                "Review assist unavailable: provider unavailable".to_string()
            ))
        );
    }

    #[test]
    fn hydrate_review_transient_ignores_missing_session() {
        // Arrange
        let mut session_state = empty_session_state();

        // Act
        hydrate_review_transient(
            &HashMap::new(),
            &mut session_state,
            "missing-session",
            AgentModel::Gpt56Sol,
        );

        // Assert
        assert!(session_state.sessions().is_empty());
    }

    #[test]
    fn prune_review_cache_retains_active_and_loading_entries() {
        // Arrange
        let active_session_id = SessionId::from("active-session");
        let loading_session_id = SessionId::from("loading-session");
        let mut review_cache = HashMap::from([
            (
                active_session_id.clone(),
                ReviewCacheEntry::Ready {
                    diff_hash: 1,
                    text: "active review".to_string(),
                },
            ),
            (
                "inactive-ready".into(),
                ReviewCacheEntry::Ready {
                    diff_hash: 2,
                    text: "inactive review".to_string(),
                },
            ),
            (
                "inactive-failed".into(),
                ReviewCacheEntry::Failed {
                    diff_hash: 3,
                    error: "failed review".to_string(),
                },
            ),
            ("inactive-suppressed".into(), ReviewCacheEntry::Suppressed),
            (
                loading_session_id.clone(),
                ReviewCacheEntry::Loading { diff_hash: 4 },
            ),
        ]);
        let session_state = session_state_with_stale_review(&active_session_id);

        // Act
        prune_review_cache(&mut review_cache, &session_state);

        // Assert
        assert_eq!(review_cache.len(), 2);
        assert!(review_cache.contains_key(&active_session_id));
        assert!(matches!(
            review_cache.get(&loading_session_id),
            Some(ReviewCacheEntry::Loading { diff_hash: 4 })
        ));
    }

    #[test]
    fn apply_review_updates_persists_and_evicts_inactive_success() {
        // Arrange
        let session_id = SessionId::from("session-persist-review");
        let diff_hash = 19;
        let review_text = "## Review\nPersist this finding.";
        let mut review_cache = loading_review_cache(&session_id, diff_hash);
        let mut session_state = empty_session_state();
        let review_updates = successful_review_update(&session_id, diff_hash, review_text);

        // Act
        let persistence_updates =
            apply_review_updates(&mut review_cache, &mut session_state, review_updates);

        // Assert
        assert_eq!(
            persistence_updates,
            vec![FocusedReviewPersistence {
                diff_hash: Some(diff_hash),
                session_id: session_id.clone(),
                text: Some(review_text.to_string()),
            }]
        );
        assert!(!review_cache.contains_key(&session_id));
    }

    #[test]
    fn apply_review_updates_returns_clear_for_failed_regeneration() {
        // Arrange
        let session_id = SessionId::from("session-failed-review");
        let diff_hash = 29;
        let mut review_cache = loading_review_cache(&session_id, diff_hash);
        let mut session_state = empty_session_state();
        let review_updates = HashMap::from([(
            session_id.clone(),
            ReviewUpdate {
                diff_hash,
                result: Err("provider failed".to_string()),
            },
        )]);

        // Act
        let persistence_updates =
            apply_review_updates(&mut review_cache, &mut session_state, review_updates);

        // Assert
        assert_eq!(
            persistence_updates,
            vec![FocusedReviewPersistence {
                diff_hash: None,
                session_id,
                text: None,
            }]
        );
    }

    #[test]
    fn apply_review_updates_writes_success_to_cache() {
        // Arrange
        let session_id = SessionId::from("session-cache-review");
        let diff_hash = 11;
        let review_text = "## Review\nCache-backed finding.";
        let mut review_cache = loading_review_cache(&session_id, diff_hash);
        let mut session_state = session_state_with_stale_review(&session_id);
        let review_updates = successful_review_update(&session_id, diff_hash, review_text);

        // Act
        apply_review_updates(&mut review_cache, &mut session_state, review_updates);

        // Assert
        assert!(matches!(
            review_cache.get(session_id.as_str()),
            Some(ReviewCacheEntry::Ready { text, .. }) if text == review_text
        ));
    }

    #[test]
    fn apply_review_updates_preserves_loading_row_tail_position() {
        // Arrange
        let session_id = SessionId::from("session-tail-review");
        let diff_hash = 17;
        let review_text = "## Review\nChronological finding.";
        let mut review_cache = loading_review_cache(&session_id, diff_hash);
        let mut session = SessionFixtureBuilder::new()
            .id(session_id.as_str())
            .status(Status::AgentReview)
            .build();
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Reviewing changes".to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: None,
        });
        let mut session_state = SessionState::new(
            HashMap::new(),
            vec![session],
            SelectionState::default(),
            Arc::new(RealClock),
            0,
            0,
        );
        let review_updates = successful_review_update(&session_id, diff_hash, review_text);

        // Act
        apply_review_updates(&mut review_cache, &mut session_state, review_updates);

        // Assert
        let review_message = session_state.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::Review)
            .expect("completed review should remain visible");
        assert_eq!(review_message.anchor, TransientMessageAnchor::Tail);
        assert_eq!(
            review_message.body,
            TransientMessageBody::Markdown(review_text.to_string())
        );
    }

    #[test]
    fn apply_review_updates_ignores_suppressed_auto_review_entry() {
        // Arrange
        let session_id = SessionId::from("session-suppressed-review");
        let diff_hash = 23;
        let mut review_cache = HashMap::from([(session_id.clone(), ReviewCacheEntry::Suppressed)]);
        let mut session_state = session_state_with_stale_review(&session_id);
        let review_updates =
            successful_review_update(&session_id, diff_hash, "## Review\nShould not be rendered.");

        // Act
        apply_review_updates(&mut review_cache, &mut session_state, review_updates);

        // Assert
        assert!(matches!(
            review_cache.get(session_id.as_str()),
            Some(ReviewCacheEntry::Suppressed)
        ));
    }
}
