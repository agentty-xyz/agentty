//! Idempotent retries keyed by host request IDs.
//!
//! Give the harness an [`ExecutionIdentity`], then attach a host ID to a turn
//! with `SessionTurn::host_id`. A retry with the same ID and effective request
//! returns the recorded outcome without running the model or tools again;
//! `Session::recover` reads a [`HostTurnRecord`] without executing anything.

use std::fmt;
use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::store::{AcquiredTurn, WriteRecord};
use crate::{SessionError, TurnOutcome};

/// Host assertion identifying all model and injected execution configuration.
/// Change the revision whenever behavior, endpoints, credentials' scope, or
/// injected implementations change. Never put secrets in either field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionIdentity {
    key: String,
    revision: String,
}

impl ExecutionIdentity {
    /// Creates a stable identity; both components must contain 1–256 bytes.
    ///
    /// # Errors
    /// Returns an error for empty, whitespace-only, or oversized components.
    pub fn new(key: impl Into<String>, revision: impl Into<String>) -> Result<Self, SessionError> {
        let identity = Self {
            key: key.into(),
            revision: revision.into(),
        };
        validate_identifier(&identity.key)?;
        validate_identifier(&identity.revision)?;

        Ok(identity)
    }

    /// Returns the stable host key, also used for model registry lookup.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the host revision of model and injected execution configuration.
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

/// Versioned request fingerprint supplied to atomic store acquisition.
/// Stores retain this value unchanged and compare it before admitting work.
/// Equality compares only the ID and fingerprint.
#[derive(Clone, Deserialize, Serialize)]
pub struct HostRequest {
    /// Canonical configuration behind `fingerprint`, kept on a new request
    /// so it can be rehashed under a recorded legacy tool-call limit.
    #[serde(skip)]
    configuration: Option<Box<Value>>,
    fingerprint: String,
    id: String,
    /// Tool-call limit hashed into a recorded request from before the limit
    /// was removed.
    #[serde(skip)]
    legacy_max_tool_calls: Option<NonZeroUsize>,
}

impl HostRequest {
    pub(crate) fn from_configuration(
        id: String,
        mut configuration: Value,
    ) -> Result<Self, SessionError> {
        validate_identifier(&id)?;
        configuration.sort_all_objects();

        Ok(Self {
            fingerprint: Self::hash(&configuration),
            configuration: Some(Box::new(configuration)),
            id,
            legacy_max_tool_calls: None,
        })
    }

    /// Returns the session-scoped host ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the versioned SHA-256 effective-request fingerprint.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Marks a recorded request with the tool-call limit of its stored turn
    /// options, so retries match the limit its fingerprint hashed.
    pub(crate) fn with_legacy_max_tool_calls(
        mut self,
        legacy_max_tool_calls: Option<NonZeroUsize>,
    ) -> Self {
        self.legacy_max_tool_calls = legacy_max_tool_calls;

        self
    }

    /// Whether this new request repeats `recorded`. A recorded legacy limit
    /// replaces the retired constant the harness now hashes.
    fn repeats(&self, recorded: &Self) -> bool {
        let fingerprint = match (recorded.legacy_max_tool_calls, &self.configuration) {
            (Some(max_tool_calls), Some(configuration)) => {
                let mut configuration = Value::clone(configuration);
                configuration["options"]["max_tool_calls"] = json!(max_tool_calls);

                Self::hash(&configuration)
            }
            _ => self.fingerprint.clone(),
        };

        self.id == recorded.id && fingerprint == recorded.fingerprint
    }

    fn hash(configuration: &Value) -> String {
        format!(
            "v1:{}",
            hex::encode(Sha256::digest(configuration.to_string()))
        )
    }
}

impl PartialEq for HostRequest {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.fingerprint == other.fingerprint
    }
}

impl Eq for HostRequest {}

impl fmt::Debug for HostRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostRequest")
            .field("fingerprint", &self.fingerprint)
            .field("id", &self.id)
            .field("legacy_max_tool_calls", &self.legacy_max_tool_calls)
            .finish_non_exhaustive()
    }
}

/// Result of atomically classifying a request or acquiring its first attempt.
pub enum HostTurnAcquisition {
    /// New execution, with retained ownership and bounded history.
    Acquired(AcquiredTurn),
    /// An existing request. It must never be executed again.
    Recorded(HostTurnRecord),
}

impl HostTurnAcquisition {
    /// Unwraps a plain turn's reservation; only host requests are recorded.
    pub(crate) fn into_acquired(self) -> Result<AcquiredTurn, SessionError> {
        match self {
            Self::Acquired(acquired) => Ok(acquired),
            Self::Recorded(_) => Err(SessionError::HostTurnConflict),
        }
    }
}

/// Snapshot of a host request and its known write and command effects.
/// Pending records remain unknown; this is not proof that effects have stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostTurnRecord {
    /// Command intents and observed results belonging only to this turn.
    pub commands: Vec<crate::bash::CommandRecord>,
    /// Immutable model provenance; absent for turns predating model switching.
    pub model: Option<crate::store::RecordedModel>,
    /// Immutable request identity.
    pub request: HostRequest,
    /// Recorded lifecycle state and terminal result, when available.
    pub status: HostTurnStatus,
    /// Store-assigned position, distinct from the host ID.
    pub turn_position: i64,
    /// Known write intents/outcomes belonging only to this turn.
    pub writes: Vec<WriteRecord>,
}

impl HostTurnRecord {
    /// Verifies a retry against the recorded effective request.
    ///
    /// # Errors
    /// Returns a conflict if the same host ID denotes different configuration.
    pub fn check_request(&self, request: &HostRequest) -> Result<(), SessionError> {
        if !request.repeats(&self.request) {
            return Err(SessionError::HostTurnConflict);
        }

        Ok(())
    }

    pub(crate) fn into_outcome(self) -> Result<TurnOutcome, SessionError> {
        match self.status {
            HostTurnStatus::Completed(outcome) => Ok(outcome),
            HostTurnStatus::InProgress => Err(SessionError::HostTurnInProgress(Box::new(self))),
            HostTurnStatus::Failed { .. } | HostTurnStatus::Interrupted { .. } => {
                Err(SessionError::HostTurnStopped(Box::new(self)))
            }
        }
    }
}

/// Recorded host request state. A new execution always requires a new ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostTurnStatus {
    /// Reservation remains live; no second execution was started.
    InProgress,
    /// Entire original result, including sanitized activity, committed
    /// atomically.
    Completed(TurnOutcome),
    /// Failed execution, retained for inspection without automatic retry.
    Failed {
        /// Content-free failure classification.
        error_type: String,
    },
    /// Interrupted execution with potentially unknown external effects.
    Interrupted {
        /// Content-free interruption classification.
        error_type: String,
    },
}

pub(crate) fn validate_identifier(id: &str) -> Result<(), SessionError> {
    if id.trim().is_empty() || id.len() > 256 {
        return Err(SessionError::InvalidData {
            reason: "host identity must contain 1–256 bytes and not be blank".into(),
        });
    }

    Ok(())
}
