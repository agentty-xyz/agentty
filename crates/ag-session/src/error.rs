//! Errors returned by the programmatic session API.

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
}
