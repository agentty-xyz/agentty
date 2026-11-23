//! Embedder-facing failure classification.
//!
//! [`RunError`] classifies a failed run (turn), and [`HarnessError`] a failed
//! build, configuration, or session operation, by one [`ErrorKind`] and
//! whether retrying the same call later may succeed. Both convert from the
//! detailed errors the harness returns, display like them, and expose their
//! source chain; the converted error itself stays reachable through
//! `get_ref` and `into_inner`, mirroring [`std::io::Error`].

use std::error::Error;
use std::fmt;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bash::BashError;
use crate::harness::HarnessBuildError;
use crate::limit::LimitKind;
use crate::model::{ModelError, ModelErrorType, ModelRegistryError};
use crate::session::SessionError;
use crate::turn::TurnError;
use crate::{OutputSchemaError, RepositoryError, TurnInputError};

/// Stable reason that a run or harness operation failed, for embedder retry
/// and recovery policy.
///
/// Every harness failure maps to exactly one kind. New kinds may be added as
/// the harness gains failure modes. [`crate::lifecycle::TurnErrorType`]
/// remains the low-cardinality telemetry label of a failed turn; `ErrorKind`
/// is the embedder-facing classification and its intended successor.
///
/// It serializes as its [`Self::as_str`] name, so a wire `kind` is always a
/// string; [`crate::event::RunFailure`] carries the limit of
/// [`Self::BudgetExhausted`] beside it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(into = "ErrorKindName", try_from = "ErrorKindName")]
#[non_exhaustive]
pub enum ErrorKind {
    /// The caller cancelled the run.
    Cancelled,
    /// The run reached one of its [`crate::RunLimits`], or its
    /// [`crate::budget::BudgetLedger`] refused its next model request;
    /// retrying the same run would reach the same limit.
    BudgetExhausted(LimitKind),
    /// The provider limited the request rate; retry after
    /// [`RunError::retry_after`] when present.
    RateLimited,
    /// The provider was temporarily unavailable (HTTP `500`, `502`, `503`,
    /// `504`, or `529`).
    Overloaded,
    /// The provider rejected the credentials (HTTP `401` or `403`).
    Auth,
    /// The provider account's quota or credit balance is spent.
    QuotaExceeded,
    /// The request exceeds the model's context window or budget, even after
    /// every allowed reduction.
    ContextOverflow,
    /// The model or provider returned output that is unusable: invalid JSON,
    /// a schema violation, a malformed tool call, an undecodable response
    /// envelope, or an oversized or incomplete response.
    InvalidOutput,
    /// The network failed or the provider timed out the request.
    Transport,
    /// A model request failed for a reason without a finer classification,
    /// such as another provider rejection or a failure a custom
    /// [`crate::Model`] reports through [`ModelError::request`].
    Other,
    /// A repository or sandbox tool failed in a way that ends the turn.
    ToolFailed,
    /// The model kept repeating tool calls without progress after a nudge.
    Repetition,
    /// The [`crate::approval::ApprovalHandler`] did not decide on a tool
    /// call within the harness's approval timeout, so the call did not run.
    ApprovalTimeout,
    /// The session already has an active turn or matching host request, or
    /// the session store is temporarily locked or saturated.
    Busy,
    /// The request conflicts with the session's recorded state, such as an
    /// existing identifier, a reused host turn ID, or a stale handle.
    Conflict,
    /// The requested session does not exist.
    NotFound,
    /// The session store failed or holds invalid data, so the session's
    /// durable state may be incomplete.
    Store,
    /// The harness, session, sandbox, or request configuration cannot be
    /// satisfied, including capabilities the model does not support.
    Config,
    /// The run offers native-sandboxed `bash`, the sandbox cannot run on
    /// this host, and the harness was told to refuse rather than run
    /// commands without it; the error names what is missing.
    SandboxUnavailable,
}

impl ErrorKind {
    /// Returns the stable `snake_case` name of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::BudgetExhausted(_) => "budget_exhausted",
            Self::RateLimited => "rate_limited",
            Self::Overloaded => "overloaded",
            Self::Auth => "auth",
            Self::QuotaExceeded => "quota_exceeded",
            Self::ContextOverflow => "context_overflow",
            Self::InvalidOutput => "invalid_output",
            Self::Transport => "transport",
            Self::Other => "other",
            Self::ToolFailed => "tool_failed",
            Self::Repetition => "repetition",
            Self::ApprovalTimeout => "approval_timeout",
            Self::Busy => "busy",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::Store => "store",
            Self::Config => "config",
            Self::SandboxUnavailable => "sandbox_unavailable",
        }
    }

    /// Returns whether a failure of this kind is transient, so the same call
    /// may succeed when retried later.
    pub fn is_retryable(self) -> bool {
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

impl TryFrom<ErrorKindName> for ErrorKind {
    type Error = ErrorKindWireError;

    fn try_from(name: ErrorKindName) -> Result<Self, Self::Error> {
        name.with_limit(None)
    }
}

/// Stable reason that a run or harness operation failed, for embedder retry
/// and recovery policy, by its wire name.
///
/// Every harness failure maps to exactly one kind. New kinds may be added as
/// the harness gains failure modes. Data a kind carries travels beside it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ErrorKind")]
pub(crate) enum ErrorKindName {
    /// The caller cancelled the run.
    Cancelled,
    /// The run reached one of its run limits or a budget ledger cap, named
    /// beside the kind; retrying the same run would reach the same limit.
    BudgetExhausted,
    /// The provider limited the request rate; retry after
    /// [`RunError::retry_after`] when present.
    RateLimited,
    /// The provider was temporarily unavailable (HTTP `500`, `502`, `503`,
    /// `504`, or `529`).
    Overloaded,
    /// The provider rejected the credentials (HTTP `401` or `403`).
    Auth,
    /// The provider account's quota or credit balance is spent.
    QuotaExceeded,
    /// The request exceeds the model's context window or budget, even after
    /// every allowed reduction.
    ContextOverflow,
    /// The model or provider returned output that is unusable: invalid JSON,
    /// a schema violation, a malformed tool call, an undecodable response
    /// envelope, or an oversized or incomplete response.
    InvalidOutput,
    /// The network failed or the provider timed out the request.
    Transport,
    /// A model request failed for a reason without a finer classification,
    /// such as another provider rejection or a failure a custom
    /// [`crate::Model`] reports through [`ModelError::request`].
    Other,
    /// A repository or sandbox tool failed in a way that ends the turn.
    ToolFailed,
    /// The model kept repeating tool calls without progress after a nudge.
    Repetition,
    /// The approval handler did not decide on a tool call within the
    /// harness's approval timeout, so the call did not run.
    ApprovalTimeout,
    /// The session already has an active turn or matching host request, or
    /// the session store is temporarily locked or saturated.
    Busy,
    /// The request conflicts with the session's recorded state, such as an
    /// existing identifier, a reused host turn ID, or a stale handle.
    Conflict,
    /// The requested session does not exist.
    NotFound,
    /// The session store failed or holds invalid data, so the session's
    /// durable state may be incomplete.
    Store,
    /// The harness, session, sandbox, or request configuration cannot be
    /// satisfied, including capabilities the model does not support.
    Config,
    /// The run offers native-sandboxed `bash`, the sandbox cannot run on
    /// this host, and the harness was told to refuse rather than run
    /// commands without it; the error names what is missing.
    SandboxUnavailable,
}

impl ErrorKindName {
    /// Returns the kind this name and `limit` describe.
    ///
    /// # Errors
    /// Returns an error when `limit` is missing for
    /// [`Self::BudgetExhausted`] or present for another kind.
    pub(crate) fn with_limit(
        self,
        limit: Option<LimitKind>,
    ) -> Result<ErrorKind, ErrorKindWireError> {
        let kind = match (self, limit) {
            (Self::BudgetExhausted, Some(limit)) => ErrorKind::BudgetExhausted(limit),
            (Self::BudgetExhausted, None) => return Err(ErrorKindWireError::MissingLimit),
            (_, Some(_)) => return Err(ErrorKindWireError::UnexpectedLimit),
            (Self::Cancelled, None) => ErrorKind::Cancelled,
            (Self::RateLimited, None) => ErrorKind::RateLimited,
            (Self::Overloaded, None) => ErrorKind::Overloaded,
            (Self::Auth, None) => ErrorKind::Auth,
            (Self::QuotaExceeded, None) => ErrorKind::QuotaExceeded,
            (Self::ContextOverflow, None) => ErrorKind::ContextOverflow,
            (Self::InvalidOutput, None) => ErrorKind::InvalidOutput,
            (Self::Transport, None) => ErrorKind::Transport,
            (Self::Other, None) => ErrorKind::Other,
            (Self::ToolFailed, None) => ErrorKind::ToolFailed,
            (Self::Repetition, None) => ErrorKind::Repetition,
            (Self::ApprovalTimeout, None) => ErrorKind::ApprovalTimeout,
            (Self::Busy, None) => ErrorKind::Busy,
            (Self::Conflict, None) => ErrorKind::Conflict,
            (Self::NotFound, None) => ErrorKind::NotFound,
            (Self::Store, None) => ErrorKind::Store,
            (Self::Config, None) => ErrorKind::Config,
            (Self::SandboxUnavailable, None) => ErrorKind::SandboxUnavailable,
        };

        Ok(kind)
    }
}

impl From<ErrorKind> for ErrorKindName {
    fn from(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::Cancelled => Self::Cancelled,
            ErrorKind::BudgetExhausted(_) => Self::BudgetExhausted,
            ErrorKind::RateLimited => Self::RateLimited,
            ErrorKind::Overloaded => Self::Overloaded,
            ErrorKind::Auth => Self::Auth,
            ErrorKind::QuotaExceeded => Self::QuotaExceeded,
            ErrorKind::ContextOverflow => Self::ContextOverflow,
            ErrorKind::InvalidOutput => Self::InvalidOutput,
            ErrorKind::Transport => Self::Transport,
            ErrorKind::Other => Self::Other,
            ErrorKind::ToolFailed => Self::ToolFailed,
            ErrorKind::Repetition => Self::Repetition,
            ErrorKind::ApprovalTimeout => Self::ApprovalTimeout,
            ErrorKind::Busy => Self::Busy,
            ErrorKind::Conflict => Self::Conflict,
            ErrorKind::NotFound => Self::NotFound,
            ErrorKind::Store => Self::Store,
            ErrorKind::Config => Self::Config,
            ErrorKind::SandboxUnavailable => Self::SandboxUnavailable,
        }
    }
}

/// A wire error kind whose limit does not match its name.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ErrorKindWireError {
    /// `budget_exhausted` arrived without the limit it reached.
    #[error("`budget_exhausted` requires its `limit`")]
    MissingLimit,
    /// A `limit` arrived with a kind other than `budget_exhausted`.
    #[error("only `budget_exhausted` carries a `limit`")]
    UnexpectedLimit,
}

/// Implements the shared accessors, `Display`, and [`Error`] of a public
/// error type wrapping a [`Classified`] field named `classified`.
macro_rules! classified_error_accessors {
    ($error:ident, $retryable_doc:literal, $get_ref_doc:literal) => {
        impl $error {
            /// Returns the failure's classification.
            pub fn kind(&self) -> ErrorKind {
                self.classified.kind
            }

            #[doc = $retryable_doc]
            pub fn is_retryable(&self) -> bool {
                self.classified.kind.is_retryable()
            }

            /// Returns the provider-declared delay before a retry, when the
            /// failure is retryable and the provider sent `Retry-After`.
            pub fn retry_after(&self) -> Option<Duration> {
                self.classified.retry_after
            }

            #[doc = $get_ref_doc]
            pub fn get_ref(&self) -> &(dyn Error + Send + Sync + 'static) {
                self.classified.source.as_ref()
            }

            /// Consumes the classification and returns the converted error.
            pub fn into_inner(self) -> Box<dyn Error + Send + Sync> {
                self.classified.source
            }
        }

        impl fmt::Display for $error {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.classified.source, formatter)
            }
        }

        impl Error for $error {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                self.classified.source.source()
            }
        }
    };
}

/// Classified failure of one run (turn).
///
/// Use it for the result of a turn, whether returned as [`TurnError`] or as
/// [`SessionError`] by a session; use [`HarnessError`] for building a
/// harness or opening, creating, and inspecting sessions. Its `Display` and
/// [`Error::source`] are those of the converted error, which
/// [`Self::get_ref`] and [`Self::into_inner`] return.
#[derive(Debug)]
pub struct RunError {
    classified: Classified,
}

classified_error_accessors!(
    RunError,
    "Returns whether the failure is transient, so retrying the same run later may succeed.",
    "Returns the converted error, such as a [`TurnError`], for downcasting."
);

impl RunError {
    /// Classifies a run failure that has no detailed harness error type,
    /// such as an unparsable output or an unsettled effect, as `kind`.
    pub(crate) fn with_kind(kind: ErrorKind, error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            classified: Classified::new(Classification::kind(kind), Box::new(error)),
        }
    }
}

impl From<TurnError> for RunError {
    fn from(error: TurnError) -> Self {
        Self {
            classified: Classified::new(Classification::turn(&error), Box::new(error)),
        }
    }
}

impl From<SessionError> for RunError {
    fn from(error: SessionError) -> Self {
        Self {
            classified: Classified::new(Classification::session(&error), Box::new(error)),
        }
    }
}

impl From<ModelError> for RunError {
    fn from(error: ModelError) -> Self {
        Self {
            classified: Classified::new(Classification::model(&error), Box::new(error)),
        }
    }
}

/// Classified failure of a build, configuration, or session operation
/// outside a run.
///
/// Converts from [`SessionError`] and the build-time errors without exposing
/// storage-backend types; use [`RunError`] for the result of a turn. Its
/// `Display` and [`Error::source`] are those of the converted error, which
/// [`Self::get_ref`] and [`Self::into_inner`] return.
#[derive(Debug)]
pub struct HarnessError {
    classified: Classified,
}

impl HarnessError {
    /// Classifies `error` as an [`ErrorKind::Config`] failure.
    pub(crate) fn config(error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            classified: Classified::new(Classification::kind(ErrorKind::Config), Box::new(error)),
        }
    }
}

classified_error_accessors!(
    HarnessError,
    "Returns whether the failure is transient, so retrying the same operation later may succeed.",
    "Returns the converted error, such as a [`SessionError`], for downcasting."
);

impl From<SessionError> for HarnessError {
    fn from(error: SessionError) -> Self {
        Self {
            classified: Classified::new(Classification::session(&error), Box::new(error)),
        }
    }
}

impl From<OutputSchemaError> for HarnessError {
    fn from(error: OutputSchemaError) -> Self {
        Self::config(error)
    }
}

impl From<TurnInputError> for HarnessError {
    fn from(error: TurnInputError) -> Self {
        Self::config(error)
    }
}

impl From<RepositoryError> for HarnessError {
    fn from(error: RepositoryError) -> Self {
        Self::config(error)
    }
}

impl From<ModelRegistryError> for HarnessError {
    fn from(error: ModelRegistryError) -> Self {
        Self::config(error)
    }
}

impl From<HarnessBuildError> for HarnessError {
    fn from(error: HarnessBuildError) -> Self {
        Self::config(error)
    }
}

/// Classification stored with the converted error it was derived from.
#[derive(Debug)]
struct Classified {
    kind: ErrorKind,
    retry_after: Option<Duration>,
    source: Box<dyn Error + Send + Sync>,
}

impl Classified {
    fn new(classification: Classification, source: Box<dyn Error + Send + Sync>) -> Self {
        let kind = classification.kind;

        Self {
            kind,
            retry_after: classification.retry_after.filter(|_| kind.is_retryable()),
            source,
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
            TurnError::Bash(BashError::InvalidPolicy | BashError::Unavailable)
            | TurnError::RepositoryRequired
            | TurnError::ComparisonRepositoryMismatch => ErrorKind::Config,
            TurnError::CommandFailed { .. }
            | TurnError::Bash(_)
            | TurnError::Read(_)
            | TurnError::Write(_)
            | TurnError::ToolFailed { .. } => ErrorKind::ToolFailed,
            TurnError::CommandJournal { .. }
            | TurnError::MessageJournal { .. }
            | TurnError::UsageJournal { .. } => ErrorKind::Store,
            TurnError::ContextBudgetExceeded { .. } => ErrorKind::ContextOverflow,
            TurnError::RepeatedToolCalls { .. } => ErrorKind::Repetition,
            TurnError::ApprovalTimeout { .. } => ErrorKind::ApprovalTimeout,
            TurnError::SandboxUnavailable(_) => ErrorKind::SandboxUnavailable,
            TurnError::LimitReached { limit } | TurnError::BudgetRefused { limit } => {
                ErrorKind::BudgetExhausted(*limit)
            }
        };

        Self::kind(kind)
    }

    fn session(error: &SessionError) -> Self {
        let kind = match error {
            SessionError::Turn(turn) => return Self::turn(turn),
            #[cfg(feature = "sqlite")]
            SessionError::Store { source, .. }
                if source
                    .downcast_ref::<sqlx::Error>()
                    .is_some_and(is_store_contention) =>
            {
                ErrorKind::Busy
            }
            SessionError::Busy { .. } | SessionError::HostTurnInProgress(_) => ErrorKind::Busy,
            SessionError::StaleModel { .. }
            | SessionError::CheckpointStale { .. }
            | SessionError::HostTurnConflict
            | SessionError::HostTurnStopped(_)
            | SessionError::AlreadyExists { .. }
            | SessionError::OwnershipLost { .. } => ErrorKind::Conflict,
            SessionError::NotFound { .. } | SessionError::TurnNotFound { .. } => {
                ErrorKind::NotFound
            }
            SessionError::ImageInputExceedsHistory { .. } => ErrorKind::ContextOverflow,
            SessionError::TurnPersistence { .. }
            | SessionError::Checkpoint(_)
            | SessionError::Store { .. }
            | SessionError::Io(_)
            | SessionError::InvalidData { .. }
            | SessionError::Schema(_) => ErrorKind::Store,
            SessionError::UnsupportedModelHistory { .. }
            | SessionError::Registry(_)
            | SessionError::ModelSelection(_)
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
            ModelErrorType::Transport => ErrorKind::Transport,
            ModelErrorType::InvalidProviderResponse
            | ModelErrorType::InvalidResponse
            | ModelErrorType::ResponseTooLarge
            | ModelErrorType::InvalidOutput
            | ModelErrorType::InvalidToolCall => ErrorKind::InvalidOutput,
            ModelErrorType::UnsupportedOutput | ModelErrorType::UnsupportedInput => {
                ErrorKind::Config
            }
            ModelErrorType::Request => ErrorKind::Other,
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
            Some(500 | 502 | 503 | 504 | 529) => ErrorKind::Overloaded,
            _ => ErrorKind::Other,
        }
    }

    fn kind(kind: ErrorKind) -> Self {
        Self {
            kind,
            retry_after: None,
        }
    }
}

/// Returns whether a store query failed only because the database was
/// temporarily saturated or locked: a pool timeout, or `SQLite`'s
/// `SQLITE_BUSY` or `SQLITE_LOCKED` primary result code.
#[cfg(feature = "sqlite")]
fn is_store_contention(error: &sqlx::Error) -> bool {
    const SQLITE_BUSY: i32 = 5;
    const SQLITE_LOCKED: i32 = 6;

    match error {
        sqlx::Error::PoolTimedOut => true,
        sqlx::Error::Database(error) => error
            .code()
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| matches!(code & 0xff, SQLITE_BUSY | SQLITE_LOCKED)),
        _ => false,
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
