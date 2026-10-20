//! Errors returned by the programmatic session API.

use crate::TaskChainPhase;

/// Stable error returned by [`crate::SessionService`] operations.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SessionError {
    /// Persisted session data could not be converted into the public model.
    #[error("Invalid session data: {0}")]
    InvalidData(String),
    /// The requested session does not exist.
    #[error("Session not found")]
    NotFound,
    /// The host session workflow rejected or failed the requested operation.
    #[error("{0}")]
    Operation(String),
    /// The session's latest workflow is still working in this phase, or an
    /// earlier reply is still being decided; reply again after it finishes.
    #[error("The task chain agent is still working; wait for it to finish before replying")]
    TaskChainBusy(TaskChainPhase),
    /// The session owns no workflow that could receive a reply.
    #[error("Session has no task chain")]
    TaskChainNotFound,
}
