use std::collections::{HashMap, HashSet};

use super::{MergeQueue, MergeQueueProgress};
use crate::domain::session::{SessionId, Status};

fn previous_states_for(session_id: &str, status: Status) -> HashMap<SessionId, Status> {
    HashMap::from([(session_id.to_string().into(), status)])
}

fn touched_session_ids(session_id: &str) -> HashSet<SessionId> {
    HashSet::from([session_id.into()])
}

#[test]
fn test_is_queued_or_active() {
    // Arrange
    let mut queue = MergeQueue::default();
    queue.enqueue("queued".into());
    queue.set_active("active".into());

    // Act & Assert
    assert!(queue.is_queued_or_active("queued"));
    assert!(queue.is_queued_or_active("active"));
    assert!(!queue.is_queued_or_active("missing"));
}

#[test]
fn test_pop_next_follows_fifo_order() {
    // Arrange
    let mut queue = MergeQueue::default();
    queue.enqueue("session-a".into());
    queue.enqueue("session-b".into());

    // Act
    let first = queue.pop_next();
    let second = queue.pop_next();
    let third = queue.pop_next();

    // Assert
    assert_eq!(first.as_deref(), Some("session-a"));
    assert_eq!(second.as_deref(), Some("session-b"));
    assert_eq!(third, None);
}

#[test]
fn test_has_work_includes_active_and_queued_merges() {
    // Arrange
    let mut active_queue = MergeQueue::default();
    active_queue.set_active("active".into());
    let mut pending_queue = MergeQueue::default();
    pending_queue.enqueue("pending".into());
    let empty_queue = MergeQueue::default();

    // Act & Assert
    assert!(active_queue.has_work());
    assert!(pending_queue.has_work());
    assert!(!empty_queue.has_work());
}

#[test]
fn test_progress_from_status_updates_done_starts_next_and_clears_active() {
    // Arrange
    let session_id = "session-1";
    let mut queue = MergeQueue::default();
    queue.set_active(session_id.to_string().into());
    let touched_ids = touched_session_ids(session_id);
    let previous_states = previous_states_for(session_id, Status::Merging);

    // Act
    let progress =
        queue.progress_from_status_updates(Some(Status::Done), &touched_ids, &previous_states);

    // Assert
    assert_eq!(progress, MergeQueueProgress::StartNext);
    assert!(!queue.has_active());
}

#[test]
fn test_progress_from_status_updates_failure_starts_next_and_clears_active() {
    // Arrange
    let session_id = "session-1";
    let mut queue = MergeQueue::default();
    queue.set_active(session_id.to_string().into());
    let touched_ids = touched_session_ids(session_id);
    let previous_states = previous_states_for(session_id, Status::Merging);

    // Act
    let progress =
        queue.progress_from_status_updates(Some(Status::Review), &touched_ids, &previous_states);

    // Assert
    assert_eq!(progress, MergeQueueProgress::StartNext);
    assert!(!queue.has_active());
}

#[test]
fn test_progress_from_status_updates_missing_session_starts_next() {
    // Arrange
    let session_id = "session-1";
    let mut queue = MergeQueue::default();
    queue.set_active(session_id.to_string().into());
    let touched_ids = HashSet::new();
    let previous_states = HashMap::new();

    // Act
    let progress = queue.progress_from_status_updates(None, &touched_ids, &previous_states);

    // Assert
    assert_eq!(progress, MergeQueueProgress::StartNext);
    assert!(!queue.has_active());
}

#[test]
fn test_progress_from_status_updates_ignores_unrelated_batches() {
    // Arrange
    let session_id = "session-1";
    let mut queue = MergeQueue::default();
    queue.set_active(session_id.to_string().into());
    let touched_ids = HashSet::new();
    let previous_states = HashMap::new();

    // Act
    let progress =
        queue.progress_from_status_updates(Some(Status::Merging), &touched_ids, &previous_states);

    // Assert
    assert_eq!(progress, MergeQueueProgress::NoAction);
    assert!(queue.has_active());
}

#[test]
fn test_progress_from_status_updates_handles_synced_done_without_previous_merging() {
    // Arrange
    let session_id = "session-1";
    let mut queue = MergeQueue::default();
    queue.set_active(session_id.to_string().into());
    let touched_ids = touched_session_ids(session_id);
    let previous_states = HashMap::new();

    // Act
    let progress =
        queue.progress_from_status_updates(Some(Status::Done), &touched_ids, &previous_states);

    // Assert
    assert_eq!(progress, MergeQueueProgress::StartNext);
    assert!(!queue.has_active());
}
