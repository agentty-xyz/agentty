//! Agentty failure reasons decided by the harness's typed run
//! classification.

use ag_contracts::AgentError;
use ag_harness::{ErrorKind, RunError};

/// Agentty outcome of a failed harness run, decided by its [`ErrorKind`].
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RunFailure {
    /// The run was cancelled, so Agentty reports it as stopped rather than
    /// failed.
    Stopped,
    /// The run failed with this user-facing diagnostic.
    Failed(String),
}

impl RunFailure {
    /// Classifies a failed harness turn.
    ///
    /// Retryable kinds tell the user to send the prompt again instead of
    /// retrying here: the provider client already retried the request it
    /// could, and replaying the whole turn would repeat its tool effects and
    /// charge the provider-call budget again.
    pub(crate) fn from_run(error: impl Into<RunError>) -> Self {
        let error = error.into();
        if error.kind() == ErrorKind::Cancelled {
            return Self::Stopped;
        }
        let message = format!("Harness turn failed: {error}");
        if !error.is_retryable() {
            return Self::Failed(message);
        }
        let wait = error.retry_after().map_or_else(String::new, |delay| {
            format!(" after {} seconds", delay.as_secs().max(1))
        });

        Self::Failed(format!(
            "{message}\n\nThis failure is temporary ({}). Send the prompt again{wait}.",
            error.kind()
        ))
    }
}

impl From<String> for RunFailure {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<RunFailure> for AgentError {
    fn from(failure: RunFailure) -> Self {
        match failure {
            RunFailure::Stopped => {
                Self::InterruptedByUser("[Stopped] Session interrupted by user.".to_string())
            }
            RunFailure::Failed(message) => Self::Backend(message),
        }
    }
}

#[cfg(test)]
#[path = "run_failure_test.rs"]
mod tests;
