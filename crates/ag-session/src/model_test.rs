use std::path::Path;
use std::sync::Arc;

use crate::model::{SessionId, SessionStatus, activity_day_key_with_offset, session_branch};

#[test]
fn test_session_branch_uses_first_8_chars() {
    // Arrange
    let session_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

    // Act
    let branch = session_branch(session_id);

    // Assert
    assert_eq!(branch, "wt/a1b2c3d4");
}

#[test]
fn session_id_round_trips_json() {
    // Arrange
    let session_id = SessionId::from("session-1");

    // Act
    let serialized = serde_json::to_string(&session_id).expect("id should serialize");
    let deserialized =
        serde_json::from_str::<SessionId>(&serialized).expect("id should deserialize");

    // Assert
    assert_eq!(deserialized, session_id);
    assert_eq!(deserialized.as_str(), "session-1");
    assert_eq!(AsRef::<Path>::as_ref(&deserialized), Path::new("session-1"));
}

#[test]
fn session_id_supports_owned_conversions_and_comparisons() {
    // Arrange
    let source = Arc::<str>::from("session-1");
    let expected = "session-1".to_string();

    // Act
    let session_id = SessionId::from(source);
    let owned = String::from(session_id.clone());

    // Assert
    assert_eq!(session_id, expected);
    assert_eq!(session_id, &expected);
    assert_eq!(owned, expected);
}

#[test]
fn session_id_exposes_borrowed_string_views() {
    // Arrange
    let session_id = SessionId::from("session-1");

    // Act
    let as_str: &str = session_id.as_ref();
    let as_path: &Path = session_id.as_ref();
    let borrowed: &str = std::borrow::Borrow::borrow(&session_id);
    let dereferenced: &str = &session_id;

    // Assert
    assert_eq!(as_str, "session-1");
    assert_eq!(as_path, Path::new("session-1"));
    assert_eq!(borrowed, "session-1");
    assert_eq!(dereferenced.len(), 9);
    assert_eq!(session_id, *"session-1");
    assert_eq!(session_id, "session-1");
    assert_ne!(session_id, "session-2");
}

/// One lifecycle capability predicate.
type StatusCapability = fn(SessionStatus) -> bool;

#[test]
fn session_status_capabilities_follow_lifecycle_state() {
    // Arrange
    let capabilities: [(StatusCapability, Vec<SessionStatus>); 8] = [
        (
            SessionStatus::allows_chat_composer,
            vec![
                SessionStatus::Draft,
                SessionStatus::InProgress,
                SessionStatus::Review,
                SessionStatus::AgentReview,
                SessionStatus::Question,
                SessionStatus::Rebasing,
            ],
        ),
        (
            SessionStatus::allows_session_actions,
            vec![
                SessionStatus::Draft,
                SessionStatus::Review,
                SessionStatus::AgentReview,
                SessionStatus::Question,
            ],
        ),
        (
            SessionStatus::allows_diff_view,
            vec![
                SessionStatus::InProgress,
                SessionStatus::Review,
                SessionStatus::AgentReview,
                SessionStatus::Merged,
                SessionStatus::Done,
                SessionStatus::Canceled,
            ],
        ),
        (
            SessionStatus::allows_rebase_action,
            vec![
                SessionStatus::InProgress,
                SessionStatus::Review,
                SessionStatus::AgentReview,
            ],
        ),
        (
            SessionStatus::allows_review_actions,
            vec![SessionStatus::Review, SessionStatus::AgentReview],
        ),
        (SessionStatus::is_read_only, vec![SessionStatus::Merged]),
        (
            SessionStatus::allows_stacked_child_start,
            vec![SessionStatus::Review, SessionStatus::AgentReview],
        ),
        (
            SessionStatus::is_stack_branch_mutating,
            vec![
                SessionStatus::InProgress,
                SessionStatus::Question,
                SessionStatus::Queued,
                SessionStatus::Rebasing,
                SessionStatus::Merging,
                SessionStatus::Merged,
            ],
        ),
    ];

    for (capability, expected_statuses) in capabilities {
        // Act
        let statuses = SessionStatus::ALL
            .into_iter()
            .filter(|status| capability(*status))
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(statuses, expected_statuses);
    }
}

#[test]
fn session_status_round_trips_persisted_values() {
    // Arrange
    let statuses = SessionStatus::ALL;

    // Act
    let round_tripped = statuses.map(|status| {
        status
            .to_string()
            .parse::<SessionStatus>()
            .expect("status should parse")
    });

    // Assert
    assert_eq!(round_tripped, statuses);
    assert_eq!(
        "Committing"
            .parse::<SessionStatus>()
            .expect("legacy status should parse"),
        SessionStatus::InProgress
    );
    assert!("Unknown".parse::<SessionStatus>().is_err());
}

#[test]
fn merged_status_only_transitions_to_done() {
    // Arrange
    let status = SessionStatus::Merged;

    // Act / Assert
    assert!(status.can_transition_to(SessionStatus::Merged));
    assert!(status.can_transition_to(SessionStatus::Done));
    assert!(!status.can_transition_to(SessionStatus::Review));
}

#[test]
fn active_branch_work_statuses_can_transition_to_canceled() {
    // Arrange
    let statuses = [
        SessionStatus::Question,
        SessionStatus::Queued,
        SessionStatus::Rebasing,
        SessionStatus::Merging,
    ];

    // Act
    let cancellation_transitions =
        statuses.map(|status| status.can_transition_to(SessionStatus::Canceled));

    // Assert
    assert_eq!(cancellation_transitions, [true; 4]);
}

#[test]
fn terminal_continuation_is_available_for_done_and_canceled_sessions() {
    // Arrange
    let statuses = SessionStatus::ALL;

    // Act
    let continuation_statuses = statuses
        .into_iter()
        .filter(|status| status.allows_terminal_continuation())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        continuation_statuses,
        vec![SessionStatus::Done, SessionStatus::Canceled]
    );
}

#[test]
fn activity_day_key_applies_offsets() {
    // Arrange / Act / Assert
    assert_eq!(activity_day_key_with_offset(86_399, 1), 1);
    assert_eq!(activity_day_key_with_offset(0, -1), -1);
}
