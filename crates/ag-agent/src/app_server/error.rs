use std::error::Error;

use crate::app_server_transport::AppServerTransportError;

/// Typed error returned by app-server infrastructure operations.
///
/// Wraps transport failures, prompt rendering issues, lock errors, and
/// provider-specific failures so callers can distinguish error categories
/// without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum AppServerError {
    /// The user interrupted an in-flight app-server turn.
    #[error("{0}")]
    InterruptedByUser(String),

    /// The session registry mutex is poisoned.
    #[error("Failed to lock {provider} app-server session map")]
    LockPoisoned {
        /// Provider label for diagnostics.
        provider: &'static str,
    },

    /// A prompt template or protocol instruction rendering failed.
    #[error("{0}")]
    PromptRender(String),

    /// A provider-specific runtime startup or turn execution failure.
    #[error("{0}")]
    Provider(String),

    /// Both the initial attempt and one retry after restart failed.
    #[error(
        "{provider} app-server failed, then retry failed after restart: first error: \
         {first_error}; retry error: {retry_error}"
    )]
    RetryExhausted {
        /// Provider label for diagnostics.
        provider: &'static str,
        /// Error message from the first failed attempt.
        first_error: String,
        /// Error message from the retry attempt after restart.
        retry_error: String,
    },

    /// An stdio transport or process communication failure.
    #[error(transparent)]
    Transport(Box<dyn Error + Send + Sync>),
}

impl From<AppServerTransportError> for AppServerError {
    fn from(error: AppServerTransportError) -> Self {
        Self::Transport(Box::new(error))
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
