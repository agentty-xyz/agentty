//! Embedder-facing failure classification.
//!
//! [`RunError`] classifies a failed turn, and [`HarnessError`] a failed
//! session operation, by one [`ErrorKind`] and whether retrying the same
//! call later may succeed. Both convert from the detailed [`TurnError`],
//! [`SessionError`], and [`ModelError`] values the harness returns, and
//! display and chain exactly like them.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use crate::model::{ModelError, ModelErrorType};
use crate::session::SessionError;
use crate::turn::TurnError;

/// Stable reason that a run or session operation failed.
///
/// Every harness failure maps to exactly one kind. New kinds may be added as
/// the harness gains failure modes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The caller cancelled the run.
    Cancelled,
    /// The provider limited the request rate; retry after
    /// [`RunError::retry_after`] when present.
    RateLimited,
    /// The provider failed or was temporarily unavailable (HTTP `5xx`).
    Overloaded,
    /// The provider rejected the credentials (HTTP `401` or `403`).
    Auth,
    /// The provider account's quota or credit balance is spent.
    QuotaExceeded,
    /// The request exceeds the model's context window or budget, even after
    /// every allowed reduction.
    ContextOverflow,
    /// The model returned output that is unusable: invalid JSON, a schema
    /// violation, a malformed tool call, or an oversized or incomplete
    /// response.
    InvalidOutput,
    /// The network or the provider's response envelope failed in transit.
    Transport,
    /// The model request failed for a reason without a finer
    /// classification, including other provider rejections and failures a
    /// custom [`crate::Model`] reports through [`ModelError::request`].
    Model,
    /// A repository or sandbox tool failed in a way that ends the turn.
    ToolFailed,
    /// The model kept repeating tool calls without progress after a nudge.
    Repetition,
    /// The session already has an active turn or matching host request.
    Busy,
    /// The request conflicts with the session's recorded state, such as an
    /// existing identifier, a reused host turn ID, or a stale handle.
    Conflict,
    /// The requested session does not exist.
    NotFound,
    /// The session store failed or holds invalid data.
    Store,
    /// The harness, session, or request configuration cannot be satisfied,
    /// including capabilities the model does not support.
    Config,
}

impl ErrorKind {
    /// Returns the stable `snake_case` name of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::RateLimited => "rate_limited",
            Self::Overloaded => "overloaded",
            Self::Auth => "auth",
            Self::QuotaExceeded => "quota_exceeded",
            Self::ContextOverflow => "context_overflow",
            Self::InvalidOutput => "invalid_output",
            Self::Transport => "transport",
            Self::Model => "model",
            Self::ToolFailed => "tool_failed",
            Self::Repetition => "repetition",
            Self::Busy => "busy",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::Store => "store",
            Self::Config => "config",
        }
    }

    /// Returns whether a failure of this kind is transient, so the same call
    /// may succeed when retried later.
    fn is_transient(self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::Overloaded | Self::Transport | Self::Busy
        )
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Classified failure of one run (turn).
///
/// Converts from [`TurnError`], [`SessionError`], and [`ModelError`]. Its
/// `Display` and [`Error::source`] are those of the converted error.
#[derive(Debug)]
#[non_exhaustive]
pub struct RunError {
    kind: ErrorKind,
    retry_after: Option<Duration>,
    retryable: bool,
    source: Box<dyn Error + Send + Sync>,
}

impl RunError {
    /// Returns the failure's classification.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Returns whether the failure is transient, so retrying the same run
    /// later may succeed.
    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    /// Returns the provider-declared delay before a retry, when the failure
    /// is retryable and the provider sent `Retry-After`.
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    fn new(classification: Classification, source: Box<dyn Error + Send + Sync>) -> Self {
        let retryable = classification.kind.is_transient();

        Self {
            kind: classification.kind,
            retry_after: classification.retry_after.filter(|_| retryable),
            retryable,
            source,
        }
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.source, formatter)
    }
}

impl Error for RunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.source()
    }
}

impl From<TurnError> for RunError {
    fn from(error: TurnError) -> Self {
        Self::new(Classification::turn(&error), Box::new(error))
    }
}

impl From<SessionError> for RunError {
    fn from(error: SessionError) -> Self {
        Self::new(Classification::session(&error), Box::new(error))
    }
}

impl From<ModelError> for RunError {
    fn from(error: ModelError) -> Self {
        Self::new(Classification::model(&error), Box::new(error))
    }
}

/// Classified failure of a harness or session operation outside a run.
///
/// Converts from [`SessionError`] without exposing storage-backend types.
/// Its `Display` and [`Error::source`] are those of the converted error.
#[derive(Debug)]
#[non_exhaustive]
pub struct HarnessError {
    kind: ErrorKind,
    source: Box<dyn Error + Send + Sync>,
}

impl HarnessError {
    /// Returns the failure's classification.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Returns whether the failure is transient, so retrying the same
    /// operation later may succeed.
    pub fn is_retryable(&self) -> bool {
        self.kind.is_transient()
    }
}

impl fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.source, formatter)
    }
}

impl Error for HarnessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.source()
    }
}

impl From<SessionError> for HarnessError {
    fn from(error: SessionError) -> Self {
        Self {
            kind: Classification::session(&error).kind,
            source: Box::new(error),
        }
    }
}

/// Kind and provider retry delay derived from one detailed error.
#[derive(Clone, Copy)]
struct Classification {
    kind: ErrorKind,
    retry_after: Option<Duration>,
}

impl Classification {
    fn turn(error: &TurnError) -> Self {
        let kind = match error {
            TurnError::Model(error) => return Self::model(error),
            TurnError::Cancelled => ErrorKind::Cancelled,
            TurnError::CommandFailed { .. }
            | TurnError::Bash(_)
            | TurnError::Read(_)
            | TurnError::Write(_) => ErrorKind::ToolFailed,
            TurnError::CommandJournal { .. } | TurnError::MessageJournal { .. } => ErrorKind::Store,
            TurnError::RepositoryRequired | TurnError::ComparisonRepositoryMismatch => {
                ErrorKind::Config
            }
            TurnError::ContextBudgetExceeded { .. } => ErrorKind::ContextOverflow,
            TurnError::RepeatedToolCalls { .. } => ErrorKind::Repetition,
        };

        Self::kind(kind)
    }

    fn session(error: &SessionError) -> Self {
        let kind = match error {
            SessionError::Turn(turn) | SessionError::TurnPersistence { turn, .. } => {
                return Self::turn(turn);
            }
            SessionError::Busy { .. } | SessionError::HostTurnInProgress(_) => ErrorKind::Busy,
            SessionError::StaleModel { .. }
            | SessionError::CheckpointStale { .. }
            | SessionError::HostTurnConflict
            | SessionError::HostTurnStopped(_)
            | SessionError::AlreadyExists { .. }
            | SessionError::OwnershipLost { .. } => ErrorKind::Conflict,
            SessionError::NotFound { .. } => ErrorKind::NotFound,
            SessionError::ImageInputExceedsHistory { .. } => ErrorKind::ContextOverflow,
            SessionError::Checkpoint(_)
            | SessionError::Store { .. }
            | SessionError::Io(_)
            | SessionError::InvalidData { .. }
            | SessionError::Migration(_)
            | SessionError::QueryContext { .. }
            | SessionError::Schema(_) => ErrorKind::Store,
            SessionError::UnsupportedModelHistory { .. }
            | SessionError::Registry(_)
            | SessionError::RegistrationMismatch { .. }
            | SessionError::ExecutionIdentityRequired
            | SessionError::ModelMismatch { .. }
            | SessionError::StorageRequired => ErrorKind::Config,
        };

        Self::kind(kind)
    }

    fn model(error: &ModelError) -> Self {
        let kind = match error.error_type() {
            ModelErrorType::Provider => Self::provider_kind(error),
            ModelErrorType::ContextOverflow => ErrorKind::ContextOverflow,
            ModelErrorType::Transport | ModelErrorType::InvalidProviderResponse => {
                ErrorKind::Transport
            }
            ModelErrorType::InvalidResponse
            | ModelErrorType::ResponseTooLarge
            | ModelErrorType::InvalidOutput
            | ModelErrorType::InvalidToolCall => ErrorKind::InvalidOutput,
            ModelErrorType::UnsupportedOutput | ModelErrorType::UnsupportedInput => {
                ErrorKind::Config
            }
            ModelErrorType::Request => ErrorKind::Model,
        };

        Self {
            kind,
            retry_after: error.retry_after(),
        }
    }

    /// Classifies an unsuccessful provider response by its quota signal and
    /// HTTP status.
    fn provider_kind(error: &ModelError) -> ErrorKind {
        if error.is_quota_exceeded() {
            return ErrorKind::QuotaExceeded;
        }

        match error.http_status() {
            Some(401 | 403) => ErrorKind::Auth,
            Some(408) => ErrorKind::Transport,
            Some(429) => ErrorKind::RateLimited,
            Some(500..=599) => ErrorKind::Overloaded,
            _ => ErrorKind::Model,
        }
    }

    fn kind(kind: ErrorKind) -> Self {
        Self {
            kind,
            retry_after: None,
        }
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
