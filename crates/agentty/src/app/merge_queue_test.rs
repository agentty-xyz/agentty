use std::collections::{HashMap, HashSet};

use super::{MergeQueue, MergeQueueProgress};
use crate::domain::session::{SessionId, Status};

const ACTIVE_SESSION_ID: &str = "session-1";

/// Covers every branch of `MergeQueue::progress_from_status_updates`.
const PROGRESS_CASES: &[ProgressCase] = &[
    ProgressCase {
        active_session_id: None,
        current_active_status: None,
        expected_active_session_id: None,
        expected_progress: MergeQueueProgress::NoAction,
        name: "no active merge",
        previous_status: None,
        touches_active_session: true,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: None,
        expected_active_session_id: None,
        expected_progress: MergeQueueProgress::StartNext,
        name: "untouched active merge disappeared from the session list",
        previous_status: None,
        touches_active_session: false,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: Some(Status::Merging),
        expected_active_session_id: Some(ACTIVE_SESSION_ID),
        expected_progress: MergeQueueProgress::NoAction,
        name: "untouched active merge is still listed",
        previous_status: None,
        touches_active_session: false,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: Some(Status::Merging),
        expected_active_session_id: Some(ACTIVE_SESSION_ID),
        expected_progress: MergeQueueProgress::NoAction,
        name: "no previous status and now merging",
        previous_status: None,
        touches_active_session: true,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: Some(Status::Merging),
        expected_active_session_id: Some(ACTIVE_SESSION_ID),
        expected_progress: MergeQueueProgress::NoAction,
        name: "queued merge entered merging",
        previous_status: Some(Status::Queued),
        touches_active_session: true,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: Some(Status::Done),
        expected_active_session_id: None,
        expected_progress: MergeQueueProgress::StartNext,
        name: "synced done without previous merging status",
        previous_status: None,
        touches_active_session: true,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: Some(Status::Review),
        expected_active_session_id: None,
        expected_progress: MergeQueueProgress::StartNext,
        name: "queued merge returned to review without merging",
        previous_status: Some(Status::Queued),
        touches_active_session: true,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: Some(Status::Merging),
        expected_active_session_id: Some(ACTIVE_SESSION_ID),
        expected_progress: MergeQueueProgress::NoAction,
        name: "merge still in progress",
        previous_status: Some(Status::Merging),
        touches_active_session: true,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: Some(Status::Done),
        expected_active_session_id: None,
        expected_progress: MergeQueueProgress::StartNext,
        name: "merge completed",
        previous_status: Some(Status::Merging),
        touches_active_session: true,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: Some(Status::Review),
        expected_active_session_id: None,
        expected_progress: MergeQueueProgress::StartNext,
        name: "merge failed back to review",
        previous_status: Some(Status::Merging),
        touches_active_session: true,
    },
    ProgressCase {
        active_session_id: Some(ACTIVE_SESSION_ID),
        current_active_status: None,
        expected_active_session_id: None,
        expected_progress: MergeQueueProgress::StartNext,
        name: "merging session removed in the touched batch",
        previous_status: Some(Status::Merging),
        touches_active_session: true,
    },
];

/// Describes one `MergeQueue::progress_from_status_updates` scenario.
struct ProgressCase {
    active_session_id: Option<&'static str>,
    current_active_status: Option<Status>,
    expected_active_session_id: Option<&'static str>,
    expected_progress: MergeQueueProgress,
    name: &'static str,
    previous_status: Option<Status>,
    touches_active_session: bool,
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
fn test_progress_from_status_updates_resolves_every_branch() {
    for case in PROGRESS_CASES {
        // Arrange
        let mut queue = MergeQueue::default();
        if let Some(active_session_id) = case.active_session_id {
            queue.set_active(active_session_id.into());
        }
        let mut touched_session_ids: HashSet<SessionId> = HashSet::from(["unrelated".into()]);
        if case.touches_active_session {
            touched_session_ids.insert(ACTIVE_SESSION_ID.into());
        }
        let previous_session_states: HashMap<SessionId, Status> = case
            .previous_status
            .map(|previous_status| (ACTIVE_SESSION_ID.into(), previous_status))
            .into_iter()
            .collect();

        // Act
        let progress = queue.progress_from_status_updates(
            case.current_active_status,
            &touched_session_ids,
            &previous_session_states,
        );

        // Assert
        assert_eq!(progress, case.expected_progress, "{}", case.name);
        assert_eq!(
            queue.active_session_id(),
            case.expected_active_session_id,
            "{}",
            case.name
        );
    }
}
