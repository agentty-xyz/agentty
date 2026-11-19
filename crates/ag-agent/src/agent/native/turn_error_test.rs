use std::io;
use std::time::Duration;

use ag_contracts::{AgentError, OneShotError};
use ag_harness::{ModelError, SessionError, TurnError};

use super::NativeTurnError;

/// Hint every retryable diagnostic carries.
const RETRY_HINT: &str = "retrying later may succeed.";

#[test]
fn provider_failures_note_retryability_and_the_requested_wait() {
    // Arrange
    let cases = [
        (401, Some(Duration::from_secs(5)), None),
        (503, None, Some("(overloaded); retrying later may succeed.")),
        (
            429,
            Some(Duration::from_secs(1)),
            Some(
                "(rate_limited); retrying later may succeed. The provider asked to wait at least \
                 1 second.",
            ),
        ),
        (
            503,
            Some(Duration::from_millis(1500)),
            Some(
                "(overloaded); retrying later may succeed. The provider asked to wait at least 2 \
                 seconds.",
            ),
        ),
        (
            503,
            Some(Duration::ZERO),
            Some("(overloaded); retrying later may succeed."),
        ),
    ];

    for (status, retry_after, expected_suffix) in cases {
        // Act
        let error = NativeTurnError::from_run(ModelError::provider(
            status,
            retry_after,
            io::Error::other("provider rejected the request"),
        ));

        // Assert
        assert!(
            matches!(
                &error,
                NativeTurnError::Failed(diagnostic)
                    if diagnostic.starts_with("Harness turn failed: ")
                        && expected_suffix.map_or(
                            !diagnostic.contains(RETRY_HINT),
                            |suffix| diagnostic.ends_with(suffix),
                        )
            ),
            "{status}: {error:?}"
        );
    }
}

#[test]
fn cancelled_turn_is_stopped() {
    // Arrange
    let error = TurnError::Cancelled;

    // Act
    let turn_error = NativeTurnError::from_run(error);

    // Assert
    assert_eq!(turn_error, NativeTurnError::Stopped);
}

#[test]
fn session_operation_failures_note_only_transient_kinds() {
    // Arrange
    let busy = SessionError::Busy {
        id: "session".to_string(),
    };
    let missing = SessionError::NotFound {
        id: "session".to_string(),
    };

    // Act
    let busy = NativeTurnError::session_operation("Failed to resume the harness session")(busy);
    let missing =
        NativeTurnError::session_operation("Failed to resume the harness session")(missing);

    // Assert
    assert!(
        matches!(
            &busy,
            NativeTurnError::Failed(diagnostic)
                if diagnostic.starts_with("Failed to resume the harness session: ")
                    && diagnostic.ends_with("(busy); retrying later may succeed.")
        ),
        "{busy:?}"
    );
    assert!(
        matches!(
            &missing,
            NativeTurnError::Failed(diagnostic)
                if diagnostic.starts_with("Failed to resume the harness session: ")
                    && !diagnostic.contains(RETRY_HINT)
        ),
        "{missing:?}"
    );
}

#[test]
fn stopped_and_failed_turns_convert_to_agentty_errors() {
    // Arrange
    let failed = || NativeTurnError::from("Invalid harness worktree".to_string());

    // Act
    let session_stopped = AgentError::from(NativeTurnError::Stopped);
    let session_failed = AgentError::from(failed());
    let utility_stopped = OneShotError::from(NativeTurnError::Stopped);
    let utility_failed = OneShotError::from(failed());

    // Assert
    assert!(matches!(
        session_stopped,
        AgentError::InterruptedByUser(message) if message == "[Stopped] Session interrupted by user."
    ));
    assert!(matches!(
        session_failed,
        AgentError::Backend(message) if message == "Invalid harness worktree"
    ));
    assert_eq!(utility_stopped.to_string(), "[Stopped] Agent run canceled");
    assert_eq!(utility_failed.to_string(), "Invalid harness worktree");
}
