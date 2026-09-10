use super::{
    QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
    TransientMessageLifecycle, TransientMessageSlot, TransientMessageStore,
};

#[test]
fn queued_body_exposes_plain_display_text() {
    // Arrange
    let body =
        TransientMessageBody::Queued(QueuedAction::new(2, "sync after this turn".to_string()));

    // Act
    let text = body.text();

    // Assert
    assert_eq!(text, "sync after this turn");
}

#[test]
fn pending_indicator_only_matches_loading_and_queued_bodies() {
    // Arrange
    let bodies = [
        TransientMessageBody::Markdown("result".to_string()),
        TransientMessageBody::Plain("failure".to_string()),
        TransientMessageBody::Loading("working".to_string()),
        TransientMessageBody::Queued(QueuedAction::new(0, "waiting".to_string())),
    ];

    // Act
    let pending_indicators = bodies.map(|body| body.is_pending_indicator());

    // Assert
    assert_eq!(pending_indicators, [false, false, true, true]);
}

fn message(slot: TransientMessageSlot, text: &str, turn_position: i64) -> TransientMessage {
    TransientMessage {
        anchor: TransientMessageAnchor::AfterCompletedTurn,
        body: TransientMessageBody::Markdown(text.to_string()),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot,
        turn_position: Some(turn_position),
    }
}

#[test]
fn upsert_replaces_slot_without_moving_it() {
    // Arrange
    let mut store = TransientMessageStore::default();
    store.upsert(message(TransientMessageSlot::Review, "review", 1));
    store.upsert(message(TransientMessageSlot::WorkflowNotice, "notice", 1));

    // Act
    store.upsert(message(TransientMessageSlot::Review, "new review", 1));

    // Assert
    assert_eq!(store.messages[0].body.text(), "new review");
    assert_eq!(store.messages[1].slot, TransientMessageSlot::WorkflowNotice);
    assert_eq!(store.version(), 3);
}

#[test]
fn clear_for_new_turn_only_retracts_older_turn_scoped_messages() {
    // Arrange
    let mut store = TransientMessageStore::default();
    store.upsert(message(
        TransientMessageSlot::ReviewCommentResolution,
        "old",
        2,
    ));
    store.upsert(message(TransientMessageSlot::Review, "current", 3));
    store.upsert(TransientMessage {
        anchor: TransientMessageAnchor::AfterCompletedTurn,
        body: TransientMessageBody::Markdown("unbound".to_string()),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::WorkflowNotice,
        turn_position: None,
    });
    store.upsert(TransientMessage {
        anchor: TransientMessageAnchor::AfterCompletedTurn,
        body: TransientMessageBody::Loading("Pushing...".to_string()),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::BranchPublish,
        turn_position: Some(2),
    });

    // Act
    store.clear_for_new_turn(3);

    // Assert
    assert!(
        store
            .get(TransientMessageSlot::ReviewCommentResolution)
            .is_none()
    );
    assert!(store.get(TransientMessageSlot::Review).is_some());
    assert!(store.get(TransientMessageSlot::WorkflowNotice).is_none());
    assert!(store.get(TransientMessageSlot::BranchPublish).is_some());
}

#[test]
fn fingerprint_distinguishes_reconstructed_stores_with_matching_versions() {
    // Arrange
    let mut loading_store = TransientMessageStore::default();
    loading_store.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Reviewing changes".to_string()),
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::Review,
        turn_position: Some(1),
    });
    let mut ready_store = TransientMessageStore::default();
    ready_store.upsert(message(
        TransientMessageSlot::Review,
        "## Review\nFinding",
        1,
    ));

    // Act
    let loading_fingerprint = loading_store.fingerprint();
    let ready_fingerprint = ready_store.fingerprint();

    // Assert
    assert_eq!(loading_store.version(), ready_store.version());
    assert_ne!(loading_fingerprint, ready_fingerprint);
}

#[test]
fn fingerprint_ignores_cross_anchor_insertion_order() {
    // Arrange
    let tail_message = TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Pushing...".to_string()),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::PublishedBranchSync,
        turn_position: Some(1),
    };
    let completed_turn_message = message(TransientMessageSlot::WorkflowNotice, "commit", 1);

    let mut notice_first_store = TransientMessageStore::default();
    notice_first_store.upsert(completed_turn_message.clone());
    notice_first_store.upsert(tail_message.clone());
    let mut push_first_store = TransientMessageStore::default();
    push_first_store.upsert(tail_message);
    push_first_store.upsert(completed_turn_message);

    // Act
    let notice_first_fingerprint = notice_first_store.fingerprint();
    let push_first_fingerprint = push_first_store.fingerprint();

    // Assert
    assert_ne!(notice_first_store.messages(), push_first_store.messages());
    assert_eq!(notice_first_fingerprint, push_first_fingerprint);
}

#[test]
fn fingerprint_distinguishes_order_within_one_anchor() {
    // Arrange
    let notice = message(TransientMessageSlot::WorkflowNotice, "commit", 1);
    let review = message(TransientMessageSlot::Review, "review", 1);

    let mut notice_first_store = TransientMessageStore::default();
    notice_first_store.upsert(notice.clone());
    notice_first_store.upsert(review.clone());
    let mut review_first_store = TransientMessageStore::default();
    review_first_store.upsert(review);
    review_first_store.upsert(notice);

    // Act
    let notice_first_fingerprint = notice_first_store.fingerprint();
    let review_first_fingerprint = review_first_store.fingerprint();

    // Assert
    assert_ne!(notice_first_fingerprint, review_first_fingerprint);
}

#[test]
fn fingerprint_matches_fresh_store_after_last_message_is_retracted() {
    // Arrange
    let fresh_store = TransientMessageStore::default();
    let mut emptied_store = TransientMessageStore::default();
    emptied_store.upsert(message(TransientMessageSlot::Review, "review", 1));

    // Act
    let retracted_message = emptied_store.retract(TransientMessageSlot::Review);

    // Assert
    assert!(retracted_message.is_some());
    assert_eq!(emptied_store.messages(), []);
    assert_eq!(emptied_store.fingerprint(), fresh_store.fingerprint());
}
