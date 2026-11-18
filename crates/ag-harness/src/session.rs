//! Session protocol types shared by every session store: configuration,
//! loaded state, reservation owners, store identities, and errors.

use std::fmt;
use std::hash::{Hash, Hasher};
#[cfg(feature = "sqlite")]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

use crate::compaction::{CheckpointError, SessionCheckpoint};
use crate::context::{HistoryProjection, HistoryTurn};
use crate::recovery::ExecutionIdentity;
use crate::request_option::OutputMode;
use crate::turn_options_snapshot::StoredTurnOptionsError;
use crate::{OutputSchema, OutputSchemaError, TurnError};

/// Configuration stored when creating a persistent chat session.
#[derive(Clone, Debug)]
pub struct NewSession {
    id: String,
    output: OutputMode,
    registration_identity: Option<ExecutionIdentity>,
    system_prompt: Option<String>,
}

impl NewSession {
    /// Creates a persistent-session configuration whose default output mode
    /// is `output`, usually a JSON [`OutputSchema`].
    pub fn new(id: impl Into<String>, output: impl Into<OutputMode>) -> Self {
        Self {
            id: id.into(),
            output: output.into(),
            registration_identity: None,
            system_prompt: None,
        }
    }

    /// Sets the system prompt retained across future resumes.
    #[must_use]
    pub fn with_optional_system_prompt(mut self, system_prompt: Option<String>) -> Self {
        self.system_prompt = system_prompt;

        self
    }

    /// Sets the initial model registration captured when the session is
    /// created. `None` denotes direct construction, including legacy
    /// sessions.
    #[must_use]
    pub fn with_registration_identity(mut self, identity: Option<ExecutionIdentity>) -> Self {
        self.registration_identity = identity;

        self
    }

    /// Returns the stable application-provided session identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the registration that stores must retain unchanged across loads.
    pub fn registration_identity(&self) -> Option<&ExecutionIdentity> {
        self.registration_identity.as_ref()
    }

    /// Returns the default output mode retained by the session.
    pub fn output(&self) -> &OutputMode {
        &self.output
    }

    /// Returns the default output schema retained by the session, or `None`
    /// when it answers in plain text by default.
    pub fn schema(&self) -> Option<&OutputSchema> {
        self.output.schema()
    }

    /// Returns the optional session system prompt.
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
}

/// Stable backing-store identity for process-local admission and cleanup.
/// Independent handles for the same backing store must use equal identities.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StoreIdentity(StoreIdentityKind);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum StoreIdentityKind {
    #[cfg(feature = "sqlite")]
    File(PathBuf),
    Host(String, String),
    Temporary(u64),
}

impl StoreIdentity {
    /// Creates a host identity. Use a globally distinct backend namespace and
    /// a stable backing-store key, never a handle address or credentials.
    pub fn new(namespace: impl Into<String>, key: impl Into<String>) -> Self {
        Self(StoreIdentityKind::Host(namespace.into(), key.into()))
    }

    /// Creates an isolated process-local identity; clone it for shared handles.
    pub fn unique() -> Self {
        Self::temporary()
    }

    /// Returns the identity of the database file at `path`, or a temporary
    /// identity for an in-memory database.
    #[cfg(feature = "sqlite")]
    pub(crate) async fn for_path(path: &Path) -> Result<Self, std::io::Error> {
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Ok(Self::temporary());
        }

        tokio::fs::canonicalize(path)
            .await
            .map(|path| Self(StoreIdentityKind::File(path)))
    }

    fn temporary() -> Self {
        static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(0);

        Self(StoreIdentityKind::Temporary(
            NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed),
        ))
    }
}

/// Error returned by persistent session operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The handle predates a committed model switch, including A-to-B-to-A.
    #[error("session `{id}` model selection changed; resume a fresh handle")]
    StaleModel {
        /// Session whose selection changed.
        id: String,
    },
    /// Historical content cannot be replayed by the selected model.
    #[error("cannot switch session model: {reason}")]
    UnsupportedModelHistory {
        /// Content-free explanation of the unsupported capability.
        reason: &'static str,
    },
    /// Image-bearing input alone exceeds the session's replay budget, so no
    /// later turn could see it or any earlier history.
    #[error(
        "image input retains {input_bytes} bytes but session `{id}` replays at most \
         {max_history_bytes}; create the session with a larger Harness::max_history_bytes"
    )]
    ImageInputExceedsHistory {
        /// Session whose budget is too small.
        id: String,
        /// Retained size of the rejected input, with images at data-URL length.
        input_bytes: usize,
        /// Replay budget stored for the session.
        max_history_bytes: usize,
    },
    /// A compaction checkpoint record was invalid or exceeded its bounds.
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
    /// The session's model selection or checkpoint coverage advanced after the
    /// checkpoint was generated; nothing was published.
    #[error("session `{id}` rejected a stale checkpoint publication")]
    CheckpointStale {
        /// Session whose state advanced past the checkpoint's source.
        id: String,
    },
    /// The host selected a key absent from its registry.
    #[error(transparent)]
    Registry(#[from] crate::model::ModelRegistryError),
    /// [`crate::Session::compact_with`] could not select its model: the
    /// harness has no catalog, or the catalog cannot resolve the selector.
    #[error("cannot select the compaction model")]
    ModelSelection(#[source] crate::HarnessBuildError),
    /// The harness's registration differs from the session's current
    /// selection.
    #[error("persistent session `{id}` has a different model registration")]
    RegistrationMismatch {
        /// Requested session identifier.
        id: String,
    },
    /// Host-ID submission requires a stable assertion of execution
    /// configuration.
    #[error("host requests require Harness::execution_identity")]
    ExecutionIdentityRequired,
    /// Reusing a host ID with different effective input is forbidden.
    #[error("host turn ID conflicts with its recorded effective request")]
    HostTurnConflict,
    /// The matching request remains active; no duplicate execution was started.
    #[error("host turn is in progress")]
    HostTurnInProgress(Box<crate::recovery::HostTurnRecord>),
    /// Failed/interrupted requests are inspectable, never automatically rerun.
    #[error("host turn has stopped; an explicit new attempt requires a new ID")]
    HostTurnStopped(Box<crate::recovery::HostTurnRecord>),
    /// A host-provided store failed without requiring a SQLite error type.
    #[error("session store operation `{operation}` failed: {source}")]
    Store {
        /// Stable semantic operation name, excluding request contents.
        operation: &'static str,
        /// Original backend error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A session with the requested identifier already exists.
    #[error("persistent session `{id}` already exists")]
    AlreadyExists {
        /// Conflicting session identifier.
        id: String,
    },
    /// Another process or task is already running a turn for this session.
    #[error("persistent session `{id}` already has an active turn")]
    Busy {
        /// Busy session identifier.
        id: String,
    },
    /// A filesystem operation failed while opening the database.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Persisted data violated a session invariant.
    #[error("invalid persistent session data: {reason}")]
    InvalidData {
        /// Description of the invalid persisted value.
        reason: String,
    },
    /// The model supplied while opening a session differs from the saved model.
    #[error(
        "persistent session `{id}` uses {stored_provider}/{stored_model}, not \
         {actual_provider}/{actual_model}"
    )]
    ModelMismatch {
        /// Model supplied by the current harness.
        actual_model: String,
        /// Provider supplied by the current harness.
        actual_provider: String,
        /// Session identifier.
        id: String,
        /// Model stored with the session.
        stored_model: String,
        /// Provider stored with the session.
        stored_provider: String,
    },
    /// An embedded migration failed.
    #[cfg(feature = "sqlite")]
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    /// The requested session does not exist.
    #[error("persistent session `{id}` does not exist")]
    NotFound {
        /// Missing session identifier.
        id: String,
    },
    /// The session has no settled turn at the requested position.
    #[error("persistent session `{id}` has no settled turn {turn}")]
    TurnNotFound {
        /// Session identifier.
        id: String,
        /// Requested turn position.
        turn: u64,
    },
    /// The turn's owner token or lease is no longer valid for this store.
    #[error("persistent session `{id}` lost ownership of turn {turn_position}")]
    OwnershipLost {
        /// Session identifier whose turn lost ownership.
        id: String,
        /// Position of the turn that lost ownership.
        turn_position: i64,
    },
    /// A named SQLite operation failed.
    #[cfg(feature = "sqlite")]
    #[error("persistent session operation `{operation}` failed: {source}")]
    QueryContext {
        /// Stable semantic operation name.
        operation: &'static str,
        /// Underlying `SQLx` failure.
        #[source]
        source: sqlx::Error,
    },
    /// A persisted output schema is no longer valid.
    #[error(transparent)]
    Schema(#[from] OutputSchemaError),
    /// Session operations require a configured store.
    #[error("durable sessions require Harness::database(path) or Harness::store(store)")]
    StorageRequired,
    /// The model turn failed; durable write records remain available through
    /// [`crate::Session::writes`].
    #[error(transparent)]
    Turn(#[from] TurnError),
    /// A model turn and the attempt to persist its failure both failed.
    #[error("{turn}; additionally failed to persist the turn failure: {persistence}")]
    TurnPersistence {
        /// Failure returned by the model turn.
        #[source]
        turn: TurnError,
        /// Failure returned while recording the turn failure.
        persistence: Box<SessionError>,
    },
}

impl From<StoredTurnOptionsError> for SessionError {
    fn from(error: StoredTurnOptionsError) -> Self {
        match error {
            StoredTurnOptionsError::InvalidData { reason } => Self::InvalidData { reason },
            StoredTurnOptionsError::Json(error) => invalid_json(error),
            StoredTurnOptionsError::Schema(error) => Self::Schema(error),
        }
    }
}

/// Session configuration and bounded finished turns returned by a store.
#[derive(Clone)]
pub struct LoadedSession {
    /// Current compaction checkpoint, replayed ahead of `turns`.
    pub checkpoint: Option<SessionCheckpoint>,
    /// Highest completed, failed, or interrupted turn position, used as the
    /// next coverage boundary.
    pub latest_finished_turn: Option<i64>,
    /// Maximum history payload bytes, counted with
    /// `ModelMessage::retained_bytes`.
    pub max_history_bytes: usize,
    /// Stable model name, paired with `provider`, or both absent.
    pub model: Option<String>,
    /// Monotonic model selection generation used to reject stale handles.
    pub model_generation: i64,
    /// Session's default output mode.
    pub output: OutputMode,
    /// Projection `turns` follow: the session's stored projection, or, once
    /// the turns it replays outgrow the byte budget, the projection they
    /// were re-projected to.
    pub projection: HistoryProjection,
    /// Provider paired with the stored model name.
    pub provider: Option<String>,
    /// Current registration key/revision, or `None` for direct/legacy
    /// sessions.
    pub registration_identity: Option<ExecutionIdentity>,
    /// Session's retained system prompt.
    pub system_prompt: Option<String>,
    /// Recent finished turns after the checkpoint boundary that `projection`
    /// replays, oldest first with each turn's position, each in the form it
    /// gives the turn: all its messages, its tool traffic replaced by
    /// notices, or only its input and, for a completed turn, its final
    /// output with reasoning cleared. The turns it replays are kept while
    /// they fit the byte budget together; once they outgrow it, they are
    /// re-projected as [`HistoryProjection`] describes. A failed or
    /// interrupted turn also carries its stop record.
    pub turns: Vec<(i64, HistoryTurn)>,
}

impl LoadedSession {
    /// Returns the current selection for immutable turn provenance.
    pub fn recorded_model(&self) -> crate::store::RecordedModel {
        crate::store::RecordedModel {
            generation: self.model_generation,
            model: self.model.clone(),
            provider: self.provider.clone(),
            registration_identity: self.registration_identity.clone(),
        }
    }
}

/// Owner token identifying one reservation, independent of a storage engine.
/// Equality and hashing use only the store, session, position, and token; the
/// interruption diagnostic can change without changing reservation identity.
#[derive(Clone, Debug)]
pub struct TurnOwner {
    pub(crate) database: StoreIdentity,
    pub(crate) interruption_error_type: &'static str,
    pub(crate) session_id: String,
    pub(crate) token: Vec<u8>,
    pub(crate) turn_position: i64,
}

impl TurnOwner {
    /// Identifies a reservation with a backend-generated, non-reused owner
    /// token.
    pub fn new(
        database: StoreIdentity,
        session_id: String,
        turn_position: i64,
        token: Vec<u8>,
    ) -> Self {
        Self {
            database,
            session_id,
            turn_position,
            token,
            interruption_error_type: "interrupted",
        }
    }

    /// Backing store to which this reservation belongs.
    pub fn store_identity(&self) -> &StoreIdentity {
        &self.database
    }

    /// Session whose admission and mutations are fenced by this owner.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Persistent turn position within the session.
    pub fn turn_position(&self) -> i64 {
        self.turn_position
    }

    /// Opaque backend token to validate atomically with each mutation.
    pub fn token(&self) -> &[u8] {
        &self.token
    }

    /// Content-free diagnostic for an interrupted reservation.
    pub fn interruption_error_type(&self) -> &'static str {
        self.interruption_error_type
    }

    pub(crate) fn lost(&self) -> SessionError {
        SessionError::OwnershipLost {
            id: self.session_id.clone(),
            turn_position: self.turn_position,
        }
    }

    fn identity(&self) -> (&StoreIdentity, &str, i64, &[u8]) {
        (
            &self.database,
            &self.session_id,
            self.turn_position,
            &self.token,
        )
    }
}

impl PartialEq for TurnOwner {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for TurnOwner {}

impl Hash for TurnOwner {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

/// Returns the invalid-data error for persisted JSON that does not decode.
pub(crate) fn invalid_json(error: impl fmt::Display) -> SessionError {
    SessionError::InvalidData {
        reason: format!("invalid persistent message JSON: {error}"),
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
