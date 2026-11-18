use std::io;

use ag_contracts::AgentError;
use ag_harness::{ModelError, SessionError, TurnError};

use super::RunFailure;

#[test]
fn cancelled_turn_is_stopped_and_reported_as_interrupted() {
    // Arrange
    let error = TurnError::Cancelled;

    // Act
    let failure = RunFailure::from_run(error);

    // Assert
    assert_eq!(failure, RunFailure::Stopped);
    assert!(matches!(
        AgentError::from(failure),
        AgentError::InterruptedByUser(message) if message == "[Stopped] Session interrupted by user."
    ));
}

#[test]
fn permanent_failure_keeps_the_harness_diagnostic_without_a_retry_hint() {
    // Arrange
    let error = ModelError::request(io::Error::other("connection reset"));

    // Act
    let failure = RunFailure::from_run(error);

    // Assert
    assert!(
        matches!(
            &failure,
            RunFailure::Failed(message)
                if message.starts_with("Harness turn failed: ") && !message.contains("temporary")
        ),
        "{failure:?}"
    );
}

#[test]
fn retryable_failure_asks_to_send_the_prompt_again() {
    // Arrange
    let error = SessionError::Busy {
        id: "session".to_string(),
    };

    // Act
    let failure = RunFailure::from_run(error);

    // Assert
    assert!(
        matches!(
            &failure,
            RunFailure::Failed(message)
                if message.ends_with("\n\nThis failure is temporary (busy). Send the prompt again.")
        ),
        "{failure:?}"
    );
}

#[test]
fn setup_diagnostic_fails_as_a_backend_error() {
    // Arrange
    let failure = RunFailure::from("Invalid harness worktree".to_string());

    // Act
    let error = AgentError::from(failure);

    // Assert
    assert!(matches!(
        error,
        AgentError::Backend(message) if message == "Invalid harness worktree"
    ));
}
