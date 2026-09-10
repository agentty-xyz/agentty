use std::sync::{Arc, Mutex};

use ag_session::SessionStatus as Status;

use super::super::super::session_message::SessionTranscript;
use super::super::SessionHandles;

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
    let snapshot = handles.transcript_snapshot_with_loaded(None);

    // Assert
    assert_eq!(snapshot, None);
}

#[test]
fn test_status_all_lists_every_supported_status_in_display_order() {
    // Arrange
    let expected_statuses = [
        Status::Draft,
        Status::InProgress,
        Status::Review,
        Status::AgentReview,
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
