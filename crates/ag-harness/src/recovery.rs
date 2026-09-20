//! Host request identity and recorded outcomes, independent of store ownership.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{AcquiredTurn, SessionError, TurnOutcome, WriteRecord};

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
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct HostRequest {
    fingerprint: String,
    id: String,
}

impl HostRequest {
    pub(crate) fn from_configuration(
        id: String,
        mut configuration: Value,
    ) -> Result<Self, SessionError> {
        validate_identifier(&id)?;
        configuration.sort_all_objects();
        let fingerprint = format!(
            "v1:{}",
            hex::encode(Sha256::digest(configuration.to_string()))
        );

        Ok(Self { fingerprint, id })
    }

    /// Returns the session-scoped host ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the versioned SHA-256 effective-request fingerprint.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Result of atomically classifying a request or acquiring its first attempt.
pub enum HostTurnAcquisition {
    /// New execution, with retained ownership and bounded history.
    Acquired(AcquiredTurn),
    /// An existing request. It must never be executed again.
    Recorded(HostTurnRecord),
}

/// Snapshot of a host request and its known write and command effects.
/// Pending records remain unknown; this is not proof that effects have stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostTurnRecord {
    /// Command intents and observed results belonging only to this turn.
    pub commands: Vec<crate::CommandRecord>,
    /// Immutable model provenance; absent for turns predating model switching.
    pub model: Option<crate::RecordedModel>,
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
        if self.request != *request {
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
