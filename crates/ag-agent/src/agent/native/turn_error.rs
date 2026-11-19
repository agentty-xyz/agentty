//! Agentty failure reasons for native harness turns, decided by the
//! harness's typed failure classification.

use std::fmt::Display;
use std::time::Duration;

use ag_contracts::{AgentError, OneShotError};
use ag_harness::{ErrorKind, HarnessError, RunError};

/// Failure of a native session turn or utility run, including the setup
/// before its harness turn starts.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum NativeTurnError {
    /// The harness cancelled the turn, so Agentty reports it as stopped
    /// rather than failed.
    Stopped,
    /// The turn failed with this user-facing diagnostic.
    Failed(String),
}

impl NativeTurnError {
    /// Classifies a failed harness turn by its [`ErrorKind`].
    ///
    /// Retryable kinds only note that retrying later may succeed: the
    /// provider client already retried the request where it safely could,
    /// and replaying the whole turn here would repeat its tool effects and
    /// charge the provider-call budget again.
    pub(crate) fn from_run(error: impl Into<RunError>) -> Self {
        let error = error.into();
        if error.kind() == ErrorKind::Cancelled {
            return Self::Stopped;
        }

        Self::Failed(Self::diagnostic(
            "Harness turn failed",
            &error,
            error.kind(),
            error.is_retryable(),
            error.retry_after(),
        ))
    }

    /// Returns an error mapper that classifies a failed session operation
    /// outside the turn, such as creating, loading, resuming, or switching
    /// the harness session, with a diagnostic starting with `context`.
    pub(crate) fn session_operation<E: Into<HarnessError>>(
        context: &'static str,
    ) -> impl FnOnce(E) -> Self {
        move |error| {
            let error = error.into();

            Self::Failed(Self::diagnostic(
                context,
                &error,
                error.kind(),
                error.is_retryable(),
                error.retry_after(),
            ))
        }
    }

    /// Formats `error` after `context`, noting when a retryable `kind` may
    /// succeed later and how long the provider asked to wait.
    fn diagnostic(
        context: &str,
        error: &dyn Display,
        kind: ErrorKind,
        retryable: bool,
        retry_after: Option<Duration>,
    ) -> String {
        let diagnostic = format!("{context}: {error}");
        if !retryable {
            return diagnostic;
        }
        let wait = retry_after
            .map(|delay| delay.as_secs() + u64::from(delay.subsec_nanos() > 0))
            .filter(|seconds| *seconds > 0)
            .map_or_else(String::new, |seconds| {
                let unit = if seconds == 1 { "second" } else { "seconds" };

                format!(" The provider asked to wait at least {seconds} {unit}.")
            });

        format!(
            "{diagnostic}\n\nThis failure may be temporary ({kind}); retrying later may \
             succeed.{wait}"
        )
    }
}

impl From<String> for NativeTurnError {
    fn from(diagnostic: String) -> Self {
        Self::Failed(diagnostic)
    }
}

impl From<NativeTurnError> for AgentError {
    fn from(error: NativeTurnError) -> Self {
        match error {
            NativeTurnError::Stopped => {
                Self::InterruptedByUser("[Stopped] Session interrupted by user.".to_string())
            }
            NativeTurnError::Failed(diagnostic) => Self::Backend(diagnostic),
        }
    }
}

impl From<NativeTurnError> for OneShotError {
    fn from(error: NativeTurnError) -> Self {
        match error {
            NativeTurnError::Stopped => Self::new("[Stopped] Agent run canceled"),
            NativeTurnError::Failed(diagnostic) => Self::new(diagnostic),
        }
    }
}

#[cfg(test)]
#[path = "turn_error_test.rs"]
mod tests;
