use ag_session::SessionStatus as Status;

use super::super::SessionHandles;
use crate::domain::transient_message::{
    QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
    TransientMessageLifecycle, TransientMessageSlot,
};

#[test]
fn test_status_transition_queued_to_merging() {
    // Arrange
    let current_status = Status::Queued;

    // Act
    let can_transition = current_status.can_transition_to(Status::Merging);

    // Assert
    assert!(can_transition);
}

#[test]
fn test_status_transition_queued_to_in_progress_is_rejected() {
    // Arrange
    let current_status = Status::Queued;

    // Act
    let can_transition = current_status.can_transition_to(Status::InProgress);

    // Assert
    assert!(!can_transition);
}

#[test]
fn test_status_from_str_queued() {
    // Arrange
    let raw_status = "Queued";

    // Act
    let status = raw_status
        .parse::<Status>()
        .expect("failed to parse status");

    // Assert
    assert_eq!(status, Status::Queued);
}

#[test]
fn queued_action_snapshot_tracks_updates_and_clear() {
    // Arrange
    let handles = SessionHandles::new(Status::InProgress);
    let branch_publish = TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Queued(QueuedAction::new(1, "publish after turn".to_string())),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::BranchPublish,
        turn_position: Some(0),
    };
    let sync = TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Queued(QueuedAction::new(2, "sync after turn".to_string())),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::SyncQueue,
        turn_position: Some(0),
    };

    // Act
    handles.upsert_queued_action(branch_publish.clone());
    handles.upsert_queued_action(sync.clone());
    let queued_actions = handles.queued_action_snapshot();
    handles.resolve_queued_action(TransientMessageSlot::BranchPublish);
    let after_resolve = handles.queued_action_snapshot();
    handles.clear_queued_actions();

    // Assert
    assert_eq!(queued_actions, vec![branch_publish, sync.clone()]);
    assert_eq!(after_resolve, vec![sync]);
    assert_eq!(handles.queued_action_snapshot(), []);
}

#[test]
fn test_status_allows_chat_composer_during_idle_and_queueable_states() {
    // Arrange
    let expected_statuses = [
        Status::Draft,
        Status::InProgress,
        Status::Review,
        Status::AgentReview,
        Status::Question,
        Status::Rebasing,
    ];

    // Act
    let allowed_statuses: Vec<Status> = Status::ALL
        .into_iter()
        .filter(|status| status.allows_chat_composer())
        .collect();

    // Assert
    assert_eq!(allowed_statuses, expected_statuses);
}

#[test]
fn test_status_display_queued() {
    // Arrange
    let status = Status::Queued;

    // Act
    let displayed_status = status.to_string();

    // Assert
    assert_eq!(displayed_status, "Queued");
}
