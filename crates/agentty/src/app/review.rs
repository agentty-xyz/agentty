//! Focused review-cache and review-assist orchestration helpers.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tokio::sync::mpsc;

use super::core::AppEvent;
use super::task;
use crate::app::session_state::SessionState;
use crate::domain::agent::{AgentKind, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::review::FocusedReviewStatus;
use crate::domain::session::{Session, SessionId, Status};
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
        /// Normalized agent profile selected for this review generation.
        review_agent: ReviewAgent,
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
            Self::Loading { diff_hash, .. }
            | Self::Ready { diff_hash, .. }
            | Self::Failed { diff_hash, .. } => Some(*diff_hash),
            Self::Suppressed => None,
        }
    }

    /// Returns whether one persistence update still represents this cache
    /// generation and lifecycle state.
    pub(crate) fn matches_persistence(&self, update: &FocusedReviewPersistence) -> bool {
        let status = match self {
            Self::Loading { .. } => FocusedReviewStatus::Pending,
            Self::Ready { .. } => FocusedReviewStatus::Ready,
            Self::Failed { .. } => FocusedReviewStatus::Failed,
            Self::Suppressed => return false,
        };

        status == update.status && self.diff_hash() == update.diff_hash
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
    /// Durable generation state consumed by managed-worker orchestration.
    pub(crate) status: FocusedReviewStatus,
    /// Focused-review markdown to persist, or `None` when clearing it.
    pub(crate) text: Option<String>,
}

/// Maximum number of delayed persistence attempts after the initial
/// focused-review write fails.
pub(crate) const MAX_FOCUSED_REVIEW_PERSISTENCE_RETRIES: u8 = 3;

/// One delayed focused-review persistence attempt carried by the app event
/// reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FocusedReviewPersistenceRetry {
    /// One-based delayed retry number.
    pub(crate) attempt: u8,
    /// Focused-review generation that still needs persistence.
    pub(crate) persistence_update: FocusedReviewPersistence,
}

impl FocusedReviewPersistenceRetry {
    /// Wraps one initial write before any delayed retries have run.
    pub(crate) fn initial(persistence_update: FocusedReviewPersistence) -> Self {
        Self {
            attempt: 0,
            persistence_update,
        }
    }

    /// Returns the next bounded retry, or `None` after the retry limit.
    pub(crate) fn next(self) -> Option<Self> {
        (self.attempt < MAX_FOCUSED_REVIEW_PERSISTENCE_RETRIES).then(|| Self {
            attempt: self.attempt.saturating_add(1),
            persistence_update: self.persistence_update,
        })
    }
}

/// Focused-review loading heading shown while assist output is being prepared.
const REVIEW_LOADING_MESSAGE: &str = "Reviewing changes";

/// Agent selection, reasoning effort, and response speed used for focused
/// review generation.
pub(crate) type ReviewAgent = (AgentSelection, ReasoningLevel, SpeedMode);

/// Stable manual-review result shown when the session has no diff changes.
pub(crate) const REVIEW_NO_DIFF_MESSAGE: &str = "No diff changes found for review.";

/// Computes a deterministic `FNV-1a` hash of diff text for focused-review
/// cache invalidation.
pub(crate) fn diff_content_hash(diff: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    diff.as_bytes().iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Formats the focused-review loading status with the active agent profile.
pub(crate) fn review_loading_message(review_agent: ReviewAgent) -> String {
    let (review_selection, reasoning_level, speed_mode) = normalize_review_agent(review_agent);

    format!(
        "{REVIEW_LOADING_MESSAGE}\n{} · {} · {} · {}",
        review_agent_kind_label(review_selection.kind()),
        review_selection.model().as_str(),
        review_reasoning_label(reasoning_level),
        speed_mode.name(),
    )
}

/// Returns the title-cased provider label used in focused-review metadata.
fn review_agent_kind_label(agent_kind: AgentKind) -> &'static str {
    match agent_kind {
        AgentKind::Antigravity => "Antigravity",
        AgentKind::Gemini => "Gemini",
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
    }
}

/// Returns the human-readable reasoning label used in focused-review metadata.
fn review_reasoning_label(reasoning_level: ReasoningLevel) -> &'static str {
    match reasoning_level {
        ReasoningLevel::Low => "Low reasoning",
        ReasoningLevel::Medium => "Medium reasoning",
        ReasoningLevel::High => "High reasoning",
        ReasoningLevel::XHigh => "Extra-high reasoning",
        ReasoningLevel::Max => "Maximum reasoning",
    }
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
) {
    for session in session_state.sessions_mut() {
        hydrate_session_review_transient(review_cache, session);
    }
}

/// Rehydrates one session's focused-review cache entry into its stable display
/// slot, retracting stale display state when the cache no longer owns output.
pub(crate) fn hydrate_review_transient(
    review_cache: &HashMap<SessionId, ReviewCacheEntry>,
    session_state: &mut SessionState,
    session_id: &str,
) {
    let Some(session) = session_state.session_mut_for_id(session_id) else {
        return;
    };

    hydrate_session_review_transient(review_cache, session);
}

/// Evicts inactive completed review entries while retaining in-flight work.
///
/// A `Loading` entry must survive project switches so its eventual result can
/// still be validated and persisted. Completed entries also remain while
/// their durable write is pending; settled inactive entries are removed.
pub(crate) fn prune_review_cache(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    pending_persistence: &HashMap<SessionId, FocusedReviewPersistence>,
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
            || pending_persistence.contains_key(session_id)
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
        ReviewCacheEntry::Loading { review_agent, .. } => (
            TransientMessageAnchor::Tail,
            TransientMessageBody::Loading(review_loading_message(*review_agent)),
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
    review_agent: ReviewAgent,
    session_id: &str,
    session_folder: &Path,
    diff_hash: u64,
    review_diff: &str,
    session_chat_history: Option<&str>,
) {
    let (review_selection, reasoning_level, speed_mode) = normalize_review_agent(review_agent);

    task::TaskService::spawn_review_assist_task(task::ReviewAssistTaskInput {
        app_event_tx,
        diff_hash,
        reasoning_level,
        review_diff: review_diff.to_string(),
        review_selection,
        session_chat_history: session_chat_history.map(str::to_string),
        session_folder: session_folder.to_path_buf(),
        session_id: SessionId::from(session_id),
        speed_mode,
    });
}

pub(crate) fn normalize_review_agent(review_agent: ReviewAgent) -> ReviewAgent {
    let (review_selection, reasoning_level, speed_mode) = review_agent;
    let speed_mode = if review_selection.kind().supports_speed_mode() {
        speed_mode
    } else {
        SpeedMode::Normal
    };
    let review_selection = review_selection.compatible_with_speed_mode(speed_mode);

    (review_selection, reasoning_level, speed_mode)
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

    persistence_updates
}

/// Records a terminal focused-review failure when preparation cannot load a
/// diff for review generation.
pub(crate) fn fail_review_preparation(
    review_cache: &mut HashMap<SessionId, ReviewCacheEntry>,
    session_state: &mut SessionState,
    session_id: &SessionId,
    error: String,
) -> FocusedReviewPersistence {
    let diff_hash = diff_content_hash("");
    review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Failed { diff_hash, error },
    );
    hydrate_review_transient(review_cache, session_state, session_id);

    FocusedReviewPersistence {
        diff_hash: Some(diff_hash),
        session_id: session_id.clone(),
        status: FocusedReviewStatus::Failed,
        text: None,
    }
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
        diff_hash: Some(diff_hash),
        session_id: SessionId::from(session_id),
        status: if result.is_ok() {
            FocusedReviewStatus::Ready
        } else {
            FocusedReviewStatus::Failed
        },
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
pub(crate) fn restore_session_review_status(session_state: &mut SessionState, session_id: &str) {
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
#[path = "review_test.rs"]
mod tests;
