//! Session status action gate and transcript snapshot tests.

use std::sync::{Arc, Mutex};

use ag_session::SessionStatus as Status;

use super::super::super::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use super::super::SessionHandles;
use crate::domain::session::SessionDiffState;
use crate::test_support::SessionFixtureBuilder;

#[test]
fn test_status_transition_in_progress_to_canceled() {
    // Arrange
    let current_status = Status::InProgress;

    // Act
    let can_transition = current_status.can_transition_to(Status::Canceled);

    // Assert
    assert!(can_transition);
}

#[test]
fn test_status_allows_session_actions_for_idle_interactive_states() {
    // Arrange
    let expected_statuses = [
        Status::Draft,
        Status::Review,
        Status::AgentReview,
        Status::Watching,
        Status::Question,
    ];

    // Act
    let allowed_statuses: Vec<Status> = Status::ALL
        .into_iter()
        .filter(|status| status.allows_session_actions())
        .collect();

    // Assert
    assert_eq!(allowed_statuses, expected_statuses);
}

#[test]
fn test_transcript_snapshot_with_loaded_returns_none_for_poisoned_transcript_lock() {
    // Arrange
    let handles = SessionHandles::new_unloaded(Status::Review);
    let transcript = Arc::clone(&handles.transcript);
    let poison_transcript = |transcript: Arc<Mutex<SessionTranscript>>, should_poison: bool| {
        let _transcript = transcript
            .lock()
            .expect("transcript lock should initially be available");

        assert!(!should_poison, "poison transcript lock");
    };
    poison_transcript(Arc::clone(&transcript), false);
    let poison_result = std::thread::spawn(move || poison_transcript(transcript, true)).join();
    assert!(poison_result.is_err());

    // Act
    let snapshot = handles.transcript_snapshot_with_loaded(None, None);

    // Assert
    assert_eq!(snapshot, None);
}

#[test]
fn test_transcript_snapshot_with_loaded_reuses_only_a_matching_previous_snapshot() {
    // Arrange
    let live_transcript = SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        "live answer",
    )]);
    let stale_transcript = SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        "stale answer",
    )]);
    let handles = SessionHandles::new_with_transcript(Status::Review, live_transcript.clone());
    let previous_snapshot = live_transcript.clone();
    let previous_messages = previous_snapshot.messages().as_ptr();

    // Act
    let reused_snapshot = handles.transcript_snapshot_with_loaded(None, Some(previous_snapshot));
    let refreshed_snapshot = handles.transcript_snapshot_with_loaded(None, Some(stale_transcript));

    // Assert
    let reused_snapshot = reused_snapshot.expect("live transcript should be returned");
    assert_eq!(reused_snapshot.messages().as_ptr(), previous_messages);
    assert_eq!(refreshed_snapshot, Some(live_transcript));
}

#[test]
fn test_status_all_lists_every_supported_status_in_display_order() {
    // Arrange
    let expected_statuses = [
        Status::Draft,
        Status::InProgress,
        Status::Review,
        Status::AgentReview,
        Status::Watching,
        Status::Question,
        Status::Queued,
        Status::Rebasing,
        Status::Merging,
        Status::Merged,
        Status::Done,
        Status::Canceled,
    ];

    // Act
    let all_statuses = Status::ALL;

    // Assert
    assert_eq!(all_statuses, expected_statuses);
}

#[test]
fn diff_action_requires_an_archive_for_terminal_sessions() {
    // Arrange
    for status in Status::ALL {
        for has_archived_diff in [false, true] {
            for diff_state in [
                SessionDiffState::Unknown,
                SessionDiffState::Empty,
                SessionDiffState::Present,
            ] {
                let mut session = SessionFixtureBuilder::new().status(status).build();
                session.has_archived_diff = has_archived_diff;
                session.stats.diff_state = diff_state;

                // Act
                let allowed = session.allows_diff_view();

                // Assert
                let expected = if matches!(status, Status::Done | Status::Canceled) {
                    has_archived_diff
                } else if status == Status::InProgress {
                    diff_state == SessionDiffState::Present
                } else {
                    status.allows_diff_view() && diff_state != SessionDiffState::Empty
                };
                assert_eq!(
                    allowed, expected,
                    "{status:?} {diff_state:?} archive={has_archived_diff}"
                );
            }
        }
    }
}
