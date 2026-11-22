//! Transcript status line tests.

use ag_session::SessionStatus as Status;

use super::super::super::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use super::super::{COMMITTING_PROGRESS_LABEL, Session, status_message};
use crate::test_support::SessionFixtureBuilder;

/// Builds a session in `status` whose `slot` holds `body`.
fn session_with_transient(
    status: Status,
    slot: TransientMessageSlot,
    body: TransientMessageBody,
) -> Session {
    let mut session = SessionFixtureBuilder::new().status(status).build();
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body,
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot,
        turn_position: None,
    });

    session
}

#[test]
fn status_line_appends_the_latest_thought_to_working() {
    // Arrange
    let session = SessionFixtureBuilder::new()
        .status(Status::InProgress)
        .build();

    // Act
    let thinking = session.status_line(Some("  Reading files  "));
    let committing = session.status_line(Some(COMMITTING_PROGRESS_LABEL));
    let idle = session.status_line(Some(" "));

    // Assert
    assert_eq!(thinking.as_deref(), Some("Working... Reading files"));
    assert_eq!(committing.as_deref(), Some(COMMITTING_PROGRESS_LABEL));
    assert_eq!(idle.as_deref(), Some("Working..."));
}

#[test]
fn status_line_shows_a_running_comment_resolution_instead_of_the_thought() {
    // Arrange
    let session = session_with_transient(
        Status::InProgress,
        TransientMessageSlot::GeneratedTurnProgress,
        TransientMessageBody::Loading("Resolving 3 review comments...".to_string()),
    );

    // Act
    let line = session.status_line(Some("Inspecting files"));

    // Assert
    assert_eq!(line.as_deref(), Some("Resolving 3 review comments..."));
}

#[test]
fn status_line_shows_the_review_step_while_the_agent_reviews() {
    // Arrange
    let session = session_with_transient(
        Status::AgentReview,
        TransientMessageSlot::Review,
        TransientMessageBody::Loading("Reviewing with Codex...\nReading the diff".to_string()),
    );

    // Act
    let line = session.status_line(None);

    // Assert
    assert_eq!(
        line.as_deref(),
        Some("Reviewing with Codex...\nReading the diff")
    );
}

#[test]
fn status_line_ignores_a_transient_message_that_is_not_loading() {
    // Arrange
    let session = session_with_transient(
        Status::InProgress,
        TransientMessageSlot::GeneratedTurnProgress,
        TransientMessageBody::Plain("not loading".to_string()),
    );

    // Act
    let loading_message = session.generated_turn_progress_message();
    let line = session.status_line(None);

    // Assert
    assert_eq!(loading_message, None);
    assert_eq!(line.as_deref(), Some("Working..."));
}

#[test]
fn status_line_is_absent_for_idle_statuses() {
    // Arrange
    let idle_statuses = [
        Status::Draft,
        Status::Review,
        Status::Question,
        Status::Done,
        Status::Canceled,
    ];

    // Act
    let lines = idle_statuses.map(|status| {
        SessionFixtureBuilder::new()
            .status(status)
            .build()
            .status_line(Some("ignored"))
    });

    // Assert
    assert!(lines.iter().all(Option::is_none));
}

#[test]
fn status_message_explains_waiting_statuses() {
    // Arrange
    let statuses = [
        Status::Queued,
        Status::Rebasing,
        Status::Merging,
        Status::Merged,
        Status::Watching,
        Status::AgentReview,
    ];

    // Act
    let messages = statuses.map(|status| status_message(status, None, Some(" "), None));

    // Assert
    assert_eq!(
        messages,
        [
            "Waiting in merge queue...",
            "Rebasing...",
            "Merging...",
            "Waiting for manual local target sync...",
            "Watching review request for failing checks, comments, and conflicts...",
            "Reviewing changes...",
        ]
        .map(ToString::to_string)
    );
}
