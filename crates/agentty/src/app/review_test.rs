use std::collections::HashMap;
use std::sync::Arc;

use super::{
    FocusedReviewPersistence, FocusedReviewPersistenceRetry, ReviewAgent, ReviewCacheEntry,
    ReviewUpdate, apply_review_updates, hydrate_review_transient, hydrate_review_transients,
    normalize_review_agent, prune_review_cache, review_agent_kind_label, review_cache_from_rows,
    review_loading_message, review_reasoning_label, review_view_text,
};
use crate::app::session_state::SessionState;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::review::FocusedReviewStatus;
use crate::domain::selection::SelectionState;
use crate::domain::session::{SessionId, Status};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::infra::clock::RealClock;
use crate::infra::db::SessionFocusedReviewRow;
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

/// Builds a normal-speed review profile for hydration tests whose status
/// text is not under test.
fn test_review_agent() -> ReviewAgent {
    (
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        ReasoningLevel::High,
        SpeedMode::Normal,
    )
}

/// Builds a single loading review cache entry for one session.
fn loading_review_cache(
    session_id: &SessionId,
    diff_hash: u64,
) -> HashMap<SessionId, ReviewCacheEntry> {
    HashMap::from([(
        session_id.clone(),
        ReviewCacheEntry::Loading {
            diff_hash,
            review_agent: test_review_agent(),
        },
    )])
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
fn review_loading_message_uses_normalized_agent_profile() {
    // Arrange
    let review_agent = (
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt53CodexSpark),
        ReasoningLevel::XHigh,
        SpeedMode::Fast,
    );

    // Act
    let message = review_loading_message(review_agent);

    // Assert
    assert_eq!(
        message,
        "Reviewing changes\nCodex · gpt-5.6-sol · Extra-high reasoning · Fast"
    );
}

#[test]
fn review_agent_kind_labels_are_title_cased() {
    // Arrange
    let cases = [
        (AgentKind::Antigravity, "Antigravity"),
        (AgentKind::Gemini, "Gemini"),
        (AgentKind::Claude, "Claude"),
        (AgentKind::Codex, "Codex"),
    ];

    // Act
    let labels = cases
        .into_iter()
        .map(|(agent_kind, expected)| (review_agent_kind_label(agent_kind), expected))
        .collect::<Vec<_>>();

    // Assert
    assert!(labels.iter().all(|(actual, expected)| actual == expected));
}

#[test]
fn review_reasoning_labels_are_human_readable() {
    // Arrange
    let cases = [
        (ReasoningLevel::Low, "Low reasoning"),
        (ReasoningLevel::Medium, "Medium reasoning"),
        (ReasoningLevel::High, "High reasoning"),
        (ReasoningLevel::XHigh, "Extra-high reasoning"),
        (ReasoningLevel::Max, "Maximum reasoning"),
    ];

    // Act
    let labels = cases
        .into_iter()
        .map(|(reasoning_level, expected)| (review_reasoning_label(reasoning_level), expected))
        .collect::<Vec<_>>();

    // Assert
    assert!(labels.iter().all(|(actual, expected)| actual == expected));
}

#[test]
fn review_agent_speed_normalization_preserves_only_supported_speed() {
    // Arrange
    let supported_selection = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt53CodexSpark);
    let unsupported_selection = AgentSelection::new(
        AgentKind::Antigravity,
        AgentKind::Antigravity.default_model(),
    );

    // Act
    let supported =
        normalize_review_agent((supported_selection, ReasoningLevel::XHigh, SpeedMode::Fast));
    let unsupported = normalize_review_agent((
        unsupported_selection,
        ReasoningLevel::Medium,
        SpeedMode::Fast,
    ));

    // Assert
    assert_eq!(
        supported,
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            ReasoningLevel::XHigh,
            SpeedMode::Fast,
        )
    );
    assert_eq!(
        unsupported,
        (
            unsupported_selection,
            ReasoningLevel::Medium,
            SpeedMode::Normal,
        )
    );
}

#[test]
fn review_view_text_hides_cached_review_generation() {
    // Arrange
    let mut review_cache = HashMap::new();
    review_cache.insert(
        "session-id".into(),
        ReviewCacheEntry::Loading {
            diff_hash: 7,
            review_agent: test_review_agent(),
        },
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
fn review_cache_matches_only_current_persistence_state() {
    // Arrange
    let update = |status| FocusedReviewPersistence {
        diff_hash: Some(42),
        session_id: "session-id".into(),
        status,
        text: None,
    };
    let loading = ReviewCacheEntry::Loading {
        diff_hash: 42,
        review_agent: test_review_agent(),
    };
    let ready = ReviewCacheEntry::Ready {
        diff_hash: 42,
        text: "review".to_string(),
    };
    let failed = ReviewCacheEntry::Failed {
        diff_hash: 42,
        error: "failed".to_string(),
    };

    // Act / Assert
    assert!(loading.matches_persistence(&update(FocusedReviewStatus::Pending)));
    assert!(ready.matches_persistence(&update(FocusedReviewStatus::Ready)));
    assert!(failed.matches_persistence(&update(FocusedReviewStatus::Failed)));
    assert!(!ready.matches_persistence(&update(FocusedReviewStatus::Pending)));
    assert!(
        !ReviewCacheEntry::Suppressed.matches_persistence(&update(FocusedReviewStatus::Failed))
    );
    let mut stale = update(FocusedReviewStatus::Ready);
    stale.diff_hash = Some(41);
    assert!(!ready.matches_persistence(&stale));
}

#[test]
fn focused_review_persistence_retry_stops_after_limit() {
    // Arrange
    let persistence_update = FocusedReviewPersistence {
        diff_hash: Some(42),
        session_id: "session-id".into(),
        status: FocusedReviewStatus::Ready,
        text: Some("review".to_string()),
    };

    // Act
    let first = FocusedReviewPersistenceRetry::initial(persistence_update)
        .next()
        .expect("first retry should exist");
    let second = first.clone().next().expect("second retry should exist");
    let third = second.clone().next().expect("third retry should exist");
    let exhausted = third.clone().next();

    // Assert
    assert_eq!((first.attempt, second.attempt, third.attempt), (1, 2, 3));
    assert_eq!(exhausted, None);
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
    hydrate_review_transients(&review_cache, &mut session_state);

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
    hydrate_review_transient(&HashMap::new(), &mut session_state, &session_id);

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
    hydrate_review_transient(&review_cache, &mut session_state, &session_id);

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
    hydrate_review_transient(&review_cache, &mut session_state, &session_id);

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
    hydrate_review_transient(&HashMap::new(), &mut session_state, "missing-session");

    // Assert
    assert!(session_state.sessions().is_empty());
}

#[test]
fn prune_review_cache_retains_active_loading_and_pending_entries() {
    // Arrange
    let active_session_id = SessionId::from("active-session");
    let loading_session_id = SessionId::from("loading-session");
    let pending_session_id = SessionId::from("inactive-ready");
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
            ReviewCacheEntry::Loading {
                diff_hash: 4,
                review_agent: test_review_agent(),
            },
        ),
    ]);
    let pending_persistence = HashMap::from([(
        pending_session_id.clone(),
        FocusedReviewPersistence {
            diff_hash: Some(2),
            session_id: pending_session_id.clone(),
            status: FocusedReviewStatus::Ready,
            text: Some("inactive review".to_string()),
        },
    )]);
    let session_state = session_state_with_stale_review(&active_session_id);

    // Act
    prune_review_cache(&mut review_cache, &pending_persistence, &session_state);

    // Assert
    assert_eq!(review_cache.len(), 3);
    assert!(review_cache.contains_key(&active_session_id));
    assert!(review_cache.contains_key(&pending_session_id));
    assert!(matches!(
        review_cache.get(&loading_session_id),
        Some(ReviewCacheEntry::Loading { diff_hash: 4, .. })
    ));
}

#[test]
fn apply_review_updates_retains_inactive_success_until_persistence() {
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
            status: FocusedReviewStatus::Ready,
            text: Some(review_text.to_string()),
        }]
    );
    assert!(matches!(
        review_cache.get(&session_id),
        Some(ReviewCacheEntry::Ready { diff_hash: 19, text }) if text == review_text
    ));
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
            diff_hash: Some(diff_hash),
            session_id,
            status: FocusedReviewStatus::Failed,
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
