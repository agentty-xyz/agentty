//! Completed-turn persistence for resumable harness chats.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{SqliteConnection, SqlitePool, Transaction};
use thiserror::Error;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::model::{ModelMessage, ModelMetadata};
use crate::tool::{ReadArguments, ToolCall, WriteArguments};
use crate::{OutputSchema, OutputSchemaError, TurnError};

pub(crate) const TURN_LEASE_SECONDS: i64 = 300;
pub(crate) const TURN_LEASE_RENEWAL_INTERVAL_SECONDS: u64 = 100;

const DB_POOL_MAX_CONNECTIONS: u32 = 4;
const DB_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const TURN_SIZE_PAGE_SIZE: i64 = 64;

struct ModelIdentityRow {
    model: Option<String>,
    provider: Option<String>,
}

struct SessionMessageRow {
    kind: String,
    payload: String,
    turn_position: i64,
}

struct SessionRow {
    max_history_bytes: i64,
    model: Option<String>,
    output_schema: String,
    provider: Option<String>,
    provider_session_id: Option<String>,
    system_prompt: Option<String>,
}

struct TurnSizeRow {
    retained_bytes: i64,
    turn_position: i64,
}

/// Stored identity needed to reconstruct a built-in model client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionInfo {
    model: Option<String>,
    provider: Option<String>,
}

impl SessionInfo {
    /// Loads a session's stored model identity without constructing a harness.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the database cannot be opened or the
    /// session does not exist.
    pub async fn load(database_path: impl AsRef<Path>, id: &str) -> Result<Self, SessionError> {
        let database = Database::open(database_path.as_ref()).await?;
        let (provider, model) = database.load_model_identity(id).await?;

        Ok(Self { model, provider })
    }

    /// Returns the stored model identifier, when the session model exposed it.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Returns the stored provider identifier, when the session model exposed
    /// it.
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }
}

/// Configuration stored when creating a persistent chat session.
#[derive(Clone, Debug)]
pub(crate) struct NewSession {
    id: String,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl NewSession {
    /// Creates a persistent-session configuration.
    pub(crate) fn new(id: impl Into<String>, schema: OutputSchema) -> Self {
        Self {
            id: id.into(),
            schema,
            system_prompt: None,
        }
    }

    pub(crate) fn with_optional_system_prompt(mut self, system_prompt: Option<String>) -> Self {
        self.system_prompt = system_prompt;

        self
    }

    /// Returns the stable application-provided session identifier.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Returns the structured-output schema retained by the session.
    pub(crate) fn schema(&self) -> &OutputSchema {
        &self.schema
    }

    /// Returns the optional session system prompt.
    pub(crate) fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
}

/// Supplies Unix timestamps for persistent session writes.
pub(crate) trait TimestampSource: Send + Sync {
    /// Returns the current Unix timestamp in whole seconds.
    fn now_timestamp_seconds(&self) -> i64;
}

impl<TimestampFn> TimestampSource for TimestampFn
where
    TimestampFn: Fn() -> i64 + Send + Sync,
{
    fn now_timestamp_seconds(&self) -> i64 {
        self()
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
enum DatabaseIdentity {
    File(PathBuf),
    Temporary(u64),
}

impl DatabaseIdentity {
    async fn for_path(path: &Path) -> Result<Self, std::io::Error> {
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Ok(Self::temporary());
        }

        tokio::fs::canonicalize(path).await.map(Self::File)
    }

    fn temporary() -> Self {
        static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(0);

        Self::Temporary(NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed))
    }
}

/// Observes the cancellation boundary after a turn reservation is durable.
#[async_trait]
trait ReservationObserver: Send + Sync {
    async fn committed(&self);
}

#[async_trait]
impl ReservationObserver for () {
    async fn committed(&self) {}
}

/// SQLite database used by persistent harness sessions.
#[derive(Clone)]
pub(crate) struct Database {
    abandoned_turns: Arc<AbandonedTurnRegistry>,
    identity: DatabaseIdentity,
    pool: SqlitePool,
    reservation_observer: Arc<dyn ReservationObserver>,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl Database {
    /// Opens a SQLite database and runs embedded migrations.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory cannot be created, the
    /// database cannot be opened, or a migration fails.
    pub(crate) async fn open(path: &Path) -> Result<Self, SessionError> {
        Self::open_with_timestamp_source(path, system_timestamp_source()).await
    }

    /// Opens a SQLite database with an injected persistence timestamp source.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory cannot be created, the
    /// database cannot be opened, or a migration fails.
    pub(crate) async fn open_with_timestamp_source(
        path: &Path,
        timestamp_source: Arc<dyn TimestampSource>,
    ) -> Result<Self, SessionError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let options = connect_options(path);
        let pool = SqlitePoolOptions::new()
            .max_connections(DB_POOL_MAX_CONNECTIONS)
            .connect_with(options)
            .await
            .session_context("open persistent session database")?;

        sqlx::migrate!("./migrations").run(&pool).await?;
        let identity = DatabaseIdentity::for_path(path).await?;

        Ok(Self {
            abandoned_turns: shared_abandoned_turn_registry(),
            identity,
            pool,
            reservation_observer: Arc::new(()),
            timestamp_source,
        })
    }

    pub(crate) async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        max_history_bytes: usize,
    ) -> Result<(), SessionError> {
        if config.id.trim().is_empty() {
            return Err(SessionError::InvalidData {
                reason: "session identifier must not be empty".to_string(),
            });
        }
        let max_history_bytes =
            i64::try_from(max_history_bytes).map_err(|_| SessionError::InvalidData {
                reason: "history byte limit exceeds SQLite integer range".to_string(),
            })?;
        let output_schema = config.schema.value().to_string();
        let system_prompt = config.system_prompt.as_deref();
        let (provider, model) = metadata.as_ref().map_or((None, None), |metadata| {
            (Some(metadata.provider()), Some(metadata.model()))
        });
        let now = self.timestamp_source.now_timestamp_seconds();
        let result = sqlx::query(
            r"
INSERT INTO session (
    id, provider, model, output_schema, system_prompt, max_history_bytes, created_at, updated_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO NOTHING
",
        )
        .bind(&config.id)
        .bind(provider)
        .bind(model)
        .bind(output_schema)
        .bind(system_prompt)
        .bind(max_history_bytes)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .session_context("create persistent session")?;

        if result.rows_affected() == 0 {
            return Err(SessionError::AlreadyExists {
                id: config.id.clone(),
            });
        }

        Ok(())
    }

    pub(crate) async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError> {
        self.recover_stale_turns(id).await?;
        let row = sqlx::query_as!(
            SessionRow,
            r#"
SELECT provider,
       model,
       output_schema,
       system_prompt,
       max_history_bytes AS "max_history_bytes!: i64",
       provider_session_id
FROM session
WHERE id = ?
"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .session_context("load persistent session")?
        .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        let max_history_bytes = decode_max_history_bytes(id, row.max_history_bytes)?;
        let output_schema = serde_json::from_str::<Value>(&row.output_schema).map_err(|error| {
            SessionError::InvalidData {
                reason: format!("session `{id}` has invalid output-schema JSON: {error}"),
            }
        })?;
        let schema = OutputSchema::new(output_schema)?;
        let turns = self.load_turns(id, max_history_bytes).await?;

        Ok(LoadedSession {
            max_history_bytes,
            model: row.model,
            provider: row.provider,
            provider_session_id: row.provider_session_id,
            schema,
            system_prompt: row.system_prompt,
            turns,
        })
    }

    async fn load_model_identity(
        &self,
        id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionError> {
        let row = sqlx::query_as!(
            ModelIdentityRow,
            "SELECT provider, model FROM session WHERE id = ?",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .session_context("load persistent session model identity")?
        .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;

        Ok((row.provider, row.model))
    }

    pub(crate) async fn begin_turn(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<AcquiredTurn, SessionError> {
        let message = EncodedMessage::from_message(&ModelMessage::User(prompt.to_string()))?;

        loop {
            let acquisition = self.load_turn_acquisition(session_id).await?;
            if let Some(guard) = self
                .reserve_turn(session_id, &message, &acquisition)
                .await?
            {
                return Ok(AcquiredTurn {
                    guard,
                    provider_session_id: acquisition.provider_session_id,
                    turn_position: acquisition.turn_position,
                    turns: acquisition.turns,
                });
            }
        }
    }

    async fn load_turn_acquisition(
        &self,
        session_id: &str,
    ) -> Result<TurnAcquisition, SessionError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("load persistent session turn acquisition")?;
        let session = sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT max_history_bytes, provider_session_id FROM session WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .session_context("load persistent session turn acquisition")?
        .ok_or_else(|| SessionError::NotFound {
            id: session_id.to_string(),
        })?;
        let (max_history_bytes, provider_session_id) = session;
        let max_history_bytes = decode_max_history_bytes(session_id, max_history_bytes)?;
        let turn_position = next_turn_position(&mut transaction, session_id).await?;
        let latest_completed_turn = latest_completed_turn(&mut transaction, session_id).await?;
        let turns = load_turns_from(&mut transaction, session_id, max_history_bytes).await?;
        transaction
            .commit()
            .await
            .session_context("load persistent session turn acquisition")?;

        Ok(TurnAcquisition {
            latest_completed_turn,
            max_history_bytes,
            provider_session_id,
            turn_position,
            turns,
        })
    }

    async fn reserve_turn(
        &self,
        session_id: &str,
        message: &EncodedMessage,
        acquisition: &TurnAcquisition,
    ) -> Result<Option<TurnGuard>, SessionError> {
        let operation = "reserve persistent session turn";
        let recovery_now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await.session_context(operation)?;
        recover_stale_turns(&mut transaction, session_id, recovery_now).await?;
        let abandoned_turns = self.abandoned_turns.for_session(&self.identity, session_id);
        for owner in &abandoned_turns {
            interrupt_owned_turn(&mut transaction, owner, recovery_now)
                .await
                .session_context("recover abandoned persistent session turn")?;
        }
        let session = sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT max_history_bytes, provider_session_id FROM session WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .session_context(operation)?
        .ok_or_else(|| SessionError::NotFound {
            id: session_id.to_string(),
        })?;
        let (max_history_bytes, provider_session_id) = session;
        let max_history_bytes = decode_max_history_bytes(session_id, max_history_bytes)?;
        let turn_position = next_turn_position(&mut transaction, session_id).await?;
        let latest_completed_turn = latest_completed_turn(&mut transaction, session_id).await?;
        if max_history_bytes != acquisition.max_history_bytes
            || provider_session_id != acquisition.provider_session_id
            || turn_position != acquisition.turn_position
            || latest_completed_turn != acquisition.latest_completed_turn
        {
            transaction.commit().await.session_context(operation)?;

            return Ok(None);
        }
        let reservation_now = self.timestamp_source.now_timestamp_seconds();
        let lease_expires_at = reservation_now.saturating_add(TURN_LEASE_SECONDS);
        let result = sqlx::query_scalar::<_, Vec<u8>>(
            r"
INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at,
    owner_token
)
VALUES (?, ?, 'running', NULL, ?, ?, ?, randomblob(16))
RETURNING owner_token
",
        )
        .bind(session_id)
        .bind(acquisition.turn_position)
        .bind(lease_expires_at)
        .bind(reservation_now)
        .bind(reservation_now)
        .fetch_one(&mut *transaction)
        .await;
        let owner_token = result.map_err(|error| {
            if is_unique_violation(&error) {
                SessionError::Busy {
                    id: session_id.to_string(),
                }
            } else {
                SessionError::QueryContext {
                    operation: "begin persistent session turn",
                    source: error,
                }
            }
        })?;
        let owner = TurnOwner {
            database: self.identity.clone(),
            interruption_error_type: "interrupted",
            session_id: session_id.to_string(),
            token: owner_token,
            turn_position: acquisition.turn_position,
        };
        let mut guard = TurnGuard::new(self, owner);
        insert_message(
            &mut transaction,
            session_id,
            acquisition.turn_position,
            0,
            message,
            reservation_now,
            operation,
        )
        .await?;
        transaction.commit().await.session_context(operation)?;
        self.reservation_observer.committed().await;
        self.abandoned_turns.remove(&abandoned_turns);
        guard.activate();

        Ok(Some(guard))
    }

    pub(crate) async fn complete_turn(
        &self,
        session_id: &str,
        turn_position: i64,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
    ) -> Result<(), SessionError> {
        let encoded_messages = messages
            .iter()
            .map(EncodedMessage::from_message)
            .collect::<Result<Vec<_>, _>>()?;
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("complete persistent session turn")?;
        for (index, message) in encoded_messages.iter().enumerate() {
            let message_position = i64::try_from(index).unwrap_or(i64::MAX).saturating_add(1);
            insert_message(
                &mut transaction,
                session_id,
                turn_position,
                message_position,
                message,
                now,
                "complete persistent session turn",
            )
            .await?;
        }
        let result = sqlx::query(
            r"
UPDATE session_turn
SET status = 'completed', error_type = NULL, lease_expires_at = NULL, updated_at = ?
WHERE session_id = ? AND turn_position = ? AND status = 'running'
",
        )
        .bind(now)
        .bind(session_id)
        .bind(turn_position)
        .execute(&mut *transaction)
        .await
        .session_context("complete persistent session turn")?;
        if result.rows_affected() == 0 {
            return Err(SessionError::InvalidData {
                reason: format!("session `{session_id}` turn {turn_position} is not running"),
            });
        }
        sqlx::query(
            r"
UPDATE session
SET provider_session_id = ?, updated_at = ?
WHERE id = ?
",
        )
        .bind(provider_session_id)
        .bind(now)
        .bind(session_id)
        .execute(&mut *transaction)
        .await
        .session_context("complete persistent session turn")?;
        transaction
            .commit()
            .await
            .session_context("complete persistent session turn")?;

        Ok(())
    }

    pub(crate) async fn fail_turn(
        &self,
        session_id: &str,
        turn_position: i64,
        error: &TurnError,
    ) -> Result<(), SessionError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let error_type = format!("{:?}", error.error_type());
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("fail persistent session turn")?;
        let result = sqlx::query(
            r"
UPDATE session_turn
SET status = 'failed', error_type = ?, lease_expires_at = NULL, updated_at = ?
WHERE session_id = ? AND turn_position = ? AND status = 'running'
",
        )
        .bind(error_type)
        .bind(now)
        .bind(session_id)
        .bind(turn_position)
        .execute(&mut *transaction)
        .await
        .session_context("fail persistent session turn")?;
        if result.rows_affected() == 0 {
            return Err(SessionError::InvalidData {
                reason: format!("session `{session_id}` turn {turn_position} is not running"),
            });
        }
        sqlx::query(
            r"
UPDATE session
SET provider_session_id = NULL, updated_at = ?
WHERE id = ?
",
        )
        .bind(now)
        .bind(session_id)
        .execute(&mut *transaction)
        .await
        .session_context("fail persistent session turn")?;
        transaction
            .commit()
            .await
            .session_context("fail persistent session turn")?;

        Ok(())
    }

    async fn recover_stale_turns(&self, session_id: &str) -> Result<(), SessionError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("recover stale persistent session turns")?;
        recover_stale_turns(&mut transaction, session_id, now).await?;
        transaction
            .commit()
            .await
            .session_context("recover stale persistent session turns")?;

        Ok(())
    }

    async fn load_turns(
        &self,
        session_id: &str,
        max_history_bytes: usize,
    ) -> Result<Vec<Vec<ModelMessage>>, SessionError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .session_context("load persistent session history")?;

        load_turns_from(&mut connection, session_id, max_history_bytes).await
    }
}

/// Error returned by persistent session operations.
#[derive(Debug, Error)]
pub enum SessionError {
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
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    /// The requested session does not exist.
    #[error("persistent session `{id}` does not exist")]
    NotFound {
        /// Missing session identifier.
        id: String,
    },
    /// The active turn's lease was recovered by another owner.
    #[error("persistent session `{id}` lost ownership of turn {turn_position}")]
    OwnershipLost {
        /// Session identifier whose turn lost ownership.
        id: String,
        /// Position of the turn that lost ownership.
        turn_position: i64,
    },
    /// A named SQLite operation failed.
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
    /// Durable session operations require a configured SQLite database.
    #[error("durable sessions require Harness::database(path)")]
    StorageRequired,
    /// The model turn failed before it could be persisted.
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

pub(crate) struct AcquiredTurn {
    pub(crate) guard: TurnGuard,
    pub(crate) provider_session_id: Option<String>,
    pub(crate) turn_position: i64,
    pub(crate) turns: Vec<Vec<ModelMessage>>,
}

pub(crate) struct LoadedSession {
    pub(crate) max_history_bytes: usize,
    pub(crate) model: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) provider_session_id: Option<String>,
    pub(crate) schema: OutputSchema,
    pub(crate) system_prompt: Option<String>,
    pub(crate) turns: Vec<Vec<ModelMessage>>,
}

struct TurnAcquisition {
    latest_completed_turn: Option<i64>,
    max_history_bytes: usize,
    provider_session_id: Option<String>,
    turn_position: i64,
    turns: Vec<Vec<ModelMessage>>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct TurnOwner {
    database: DatabaseIdentity,
    interruption_error_type: &'static str,
    session_id: String,
    token: Vec<u8>,
    turn_position: i64,
}

#[derive(Default)]
struct AbandonedTurnRegistry {
    owners: Mutex<HashSet<TurnOwner>>,
}

impl AbandonedTurnRegistry {
    fn for_session(&self, database: &DatabaseIdentity, session_id: &str) -> Vec<TurnOwner> {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|owner| &owner.database == database && owner.session_id == session_id)
            .cloned()
            .collect()
    }

    fn register(&self, owner: TurnOwner) {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(owner);
    }

    fn remove(&self, owners: &[TurnOwner]) {
        let mut registered = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for owner in owners {
            registered.remove(owner);
        }
    }
}

fn shared_abandoned_turn_registry() -> Arc<AbandonedTurnRegistry> {
    static REGISTRY: OnceLock<Arc<AbandonedTurnRegistry>> = OnceLock::new();

    Arc::clone(REGISTRY.get_or_init(Arc::default))
}

pub(crate) struct TurnGuard {
    armed: bool,
    owner: TurnOwner,
    ownership_failure: Option<oneshot::Receiver<SessionError>>,
    pool: SqlitePool,
    registry: Arc<AbandonedTurnRegistry>,
    renewal_stop: Option<oneshot::Sender<()>>,
    renewal_task: Option<JoinHandle<()>>,
    runtime: tokio::runtime::Handle,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl TurnGuard {
    fn new(database: &Database, owner: TurnOwner) -> Self {
        Self {
            armed: true,
            owner,
            ownership_failure: None,
            pool: database.pool.clone(),
            registry: Arc::clone(&database.abandoned_turns),
            renewal_stop: None,
            renewal_task: None,
            runtime: tokio::runtime::Handle::current(),
            timestamp_source: Arc::clone(&database.timestamp_source),
        }
    }

    fn activate(&mut self) {
        self.owner.interruption_error_type = "cancelled";
        let owner = self.owner.clone();
        let interval = Duration::from_secs(TURN_LEASE_RENEWAL_INTERVAL_SECONDS);
        let first_renewal = Instant::now() + interval;
        let pool = self.pool.clone();
        let timestamp_source = Arc::clone(&self.timestamp_source);
        let (renewal_stop, mut stop_requested) = oneshot::channel();
        let (ownership_failed, ownership_failure) = oneshot::channel();
        self.renewal_stop = Some(renewal_stop);
        self.ownership_failure = Some(ownership_failure);
        self.renewal_task = Some(self.runtime.spawn(async move {
            let mut ticker = tokio::time::interval_at(first_renewal, interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let now = timestamp_source.now_timestamp_seconds();
                        let failure = match renew_owned_turn(&pool, &owner, now).await {
                            Ok(true) => continue,
                            Ok(false) => SessionError::OwnershipLost {
                                id: owner.session_id.clone(),
                                turn_position: owner.turn_position,
                            },
                            Err(source) => SessionError::QueryContext {
                                operation: "renew persistent session turn lease",
                                source,
                            },
                        };
                        let _ = ownership_failed.send(failure);

                        break;
                    }
                    _ = &mut stop_requested => break,
                }
            }
        }));
    }

    pub(crate) async fn ownership_failure(&mut self) -> SessionError {
        let Some(failure) = self.ownership_failure.take() else {
            return SessionError::OwnershipLost {
                id: self.owner.session_id.clone(),
                turn_position: self.owner.turn_position,
            };
        };

        failure
            .await
            .unwrap_or_else(|_| SessionError::OwnershipLost {
                id: self.owner.session_id.clone(),
                turn_position: self.owner.turn_position,
            })
    }

    pub(crate) fn disarm(&mut self) {
        self.stop_renewal();
        self.armed = false;
    }

    pub(crate) fn mark_interrupted(&mut self) {
        self.owner.interruption_error_type = "interrupted";
    }

    fn stop_renewal(&mut self) {
        if let Some(stop) = self.renewal_stop.take() {
            let _ = stop.send(());
        }
        self.renewal_task.take();
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.stop_renewal();
        if !self.armed {
            return;
        }
        let owner = self.owner.clone();
        self.registry.register(owner.clone());

        let pool = self.pool.clone();
        let registry = Arc::clone(&self.registry);
        let timestamp_source = Arc::clone(&self.timestamp_source);
        std::mem::drop(self.runtime.spawn(async move {
            let result = async {
                let mut transaction = pool.begin().await?;
                interrupt_owned_turn(
                    &mut transaction,
                    &owner,
                    timestamp_source.now_timestamp_seconds(),
                )
                .await?;
                transaction.commit().await
            }
            .await;
            if result.is_ok() {
                registry.remove(std::slice::from_ref(&owner));
            }
        }));
    }
}

struct EncodedMessage {
    kind: &'static str,
    payload: String,
    retained_bytes: i64,
}

impl EncodedMessage {
    fn from_message(message: &ModelMessage) -> Result<Self, SessionError> {
        let (kind, payload) = match message {
            ModelMessage::Assistant(content) => ("assistant", serialize_payload(content)),
            ModelMessage::AssistantReasoning {
                content,
                reasoning_content,
            } => (
                "assistant_reasoning",
                serialize_payload(&StoredAssistantReasoning {
                    content,
                    reasoning_content,
                }),
            ),
            ModelMessage::AssistantToolCall(call) => (
                "assistant_tool_call",
                serialize_payload(&StoredToolCall::from_call(call)?),
            ),
            ModelMessage::AssistantToolCalls(calls) => (
                "assistant_tool_calls",
                calls
                    .iter()
                    .map(StoredToolCall::from_call)
                    .collect::<Result<Vec<_>, _>>()
                    .and_then(|calls| serialize_payload(&calls)),
            ),
            ModelMessage::System(_) => {
                return Err(SessionError::InvalidData {
                    reason: "system prompts must be stored on the session".to_string(),
                });
            }
            ModelMessage::ToolResult {
                call_id,
                content,
                name,
            } => (
                "tool_result",
                serialize_payload(&StoredToolResult {
                    call_id,
                    content,
                    name,
                }),
            ),
            ModelMessage::User(content) => ("user", serialize_payload(content)),
        };
        let payload = payload?;
        let retained_bytes = i64::try_from(message.retained_bytes()).unwrap_or(i64::MAX);

        Ok(Self {
            kind,
            payload,
            retained_bytes,
        })
    }

    fn into_message(kind: &str, payload: &str) -> Result<ModelMessage, SessionError> {
        match kind {
            "assistant" => deserialize_payload(payload).map(ModelMessage::Assistant),
            "assistant_reasoning" => {
                let assistant = deserialize_payload::<StoredAssistantReasoningOwned>(payload)?;

                Ok(ModelMessage::AssistantReasoning {
                    content: assistant.content,
                    reasoning_content: assistant.reasoning_content,
                })
            }
            "assistant_tool_call" => deserialize_payload::<StoredToolCall>(payload)?
                .into_call()
                .map(ModelMessage::AssistantToolCall),
            "assistant_tool_calls" => deserialize_payload::<Vec<StoredToolCall>>(payload)?
                .into_iter()
                .map(StoredToolCall::into_call)
                .collect::<Result<Vec<_>, _>>()
                .map(ModelMessage::AssistantToolCalls),
            "tool_result" => {
                let result = deserialize_payload::<StoredToolResultOwned>(payload)?;

                Ok(ModelMessage::ToolResult {
                    call_id: result.call_id,
                    content: result.content,
                    name: result.name,
                })
            }
            "user" => deserialize_payload(payload).map(ModelMessage::User),
            _ => Err(SessionError::InvalidData {
                reason: format!("unknown persistent message kind `{kind}`"),
            }),
        }
    }
}

#[derive(Serialize)]
struct StoredAssistantReasoning<'a> {
    content: &'a str,
    reasoning_content: &'a str,
}

#[derive(Deserialize)]
struct StoredAssistantReasoningOwned {
    content: String,
    reasoning_content: String,
}

#[derive(Deserialize, Serialize)]
struct StoredToolCall {
    arguments: Value,
    id: String,
    name: String,
    reasoning_content: Option<String>,
}

impl StoredToolCall {
    fn from_call(call: &ToolCall) -> Result<Self, SessionError> {
        let arguments = call
            .arguments_json()
            .map_err(|error| invalid_json(&error))?;
        let arguments = serde_json::from_str(&arguments).map_err(|error| invalid_json(&error))?;

        Ok(Self {
            arguments,
            id: call.id().to_string(),
            name: call.name().to_string(),
            reasoning_content: call.reasoning_content().map(str::to_string),
        })
    }

    fn into_call(self) -> Result<ToolCall, SessionError> {
        match self.name.as_str() {
            "read" => serde_json::from_value::<ReadArguments>(self.arguments)
                .map(|arguments| ToolCall::read(self.id, arguments, self.reasoning_content))
                .map_err(|error| invalid_json(&error)),
            "write" => serde_json::from_value::<WriteArguments>(self.arguments)
                .map(|arguments| ToolCall::write(self.id, arguments, self.reasoning_content))
                .map_err(|error| invalid_json(&error)),
            _ => Err(SessionError::InvalidData {
                reason: format!("unknown persistent tool `{}`", self.name),
            }),
        }
    }
}

#[derive(Serialize)]
struct StoredToolResult<'a> {
    call_id: &'a str,
    content: &'a str,
    name: &'a str,
}

#[derive(Deserialize)]
struct StoredToolResultOwned {
    call_id: String,
    content: String,
    name: String,
}

trait DbResultExt<T> {
    fn session_context(self, operation: &'static str) -> Result<T, SessionError>;
}

impl<T> DbResultExt<T> for Result<T, sqlx::Error> {
    fn session_context(self, operation: &'static str) -> Result<T, SessionError> {
        self.map_err(|source| SessionError::QueryContext { operation, source })
    }
}

fn connect_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .busy_timeout(DB_BUSY_TIMEOUT)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
}

async fn load_turns_from(
    connection: &mut SqliteConnection,
    session_id: &str,
    max_history_bytes: usize,
) -> Result<Vec<Vec<ModelMessage>>, SessionError> {
    let Some(oldest_turn) =
        oldest_turn_within_budget(connection, session_id, max_history_bytes).await?
    else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query_as!(
        SessionMessageRow,
        r#"
SELECT message.turn_position AS "turn_position!: i64",
       message.kind AS "kind!: String",
       message.payload AS "payload!: String"
FROM session_message AS message
JOIN session_turn AS turn
  ON turn.session_id = message.session_id
 AND turn.turn_position = message.turn_position
WHERE message.session_id = ?
  AND message.turn_position >= ?
  AND turn.status = 'completed'
ORDER BY message.turn_position, message.message_position
"#,
        session_id,
        oldest_turn
    )
    .fetch_all(&mut *connection)
    .await
    .session_context("load persistent session history")?;
    let mut turns = Vec::<Vec<ModelMessage>>::new();
    let mut current_position = None;

    for row in rows {
        if current_position != Some(row.turn_position) {
            turns.push(Vec::new());
            current_position = Some(row.turn_position);
        }
        let message = EncodedMessage::into_message(&row.kind, &row.payload)?;
        if let Some(turn) = turns.last_mut() {
            turn.push(message);
        }
    }

    Ok(turns)
}

async fn oldest_turn_within_budget(
    connection: &mut SqliteConnection,
    session_id: &str,
    max_history_bytes: usize,
) -> Result<Option<i64>, SessionError> {
    let mut retained_bytes = 0_usize;
    let mut oldest_turn = None;
    let mut before_turn = None;

    'pages: loop {
        let turn_sizes = load_turn_size_page(connection, session_id, before_turn).await?;
        if turn_sizes.is_empty() {
            break;
        }
        let page_is_full =
            turn_sizes.len() == usize::try_from(TURN_SIZE_PAGE_SIZE).unwrap_or(usize::MAX);

        for (turn_position, turn_bytes) in turn_sizes {
            let turn_bytes =
                usize::try_from(turn_bytes).map_err(|_| SessionError::InvalidData {
                    reason: format!("session `{session_id}` has an invalid retained byte count"),
                })?;
            let next_retained_bytes = retained_bytes.saturating_add(turn_bytes);
            if next_retained_bytes > max_history_bytes {
                break 'pages;
            }
            retained_bytes = next_retained_bytes;
            oldest_turn = Some(turn_position);
            before_turn = Some(turn_position);
        }

        if !page_is_full {
            break;
        }
    }

    Ok(oldest_turn)
}

async fn load_turn_size_page(
    connection: &mut SqliteConnection,
    session_id: &str,
    before_turn: Option<i64>,
) -> Result<Vec<(i64, i64)>, SessionError> {
    let Some(inclusive_end) =
        before_turn.map_or(Some(i64::MAX), |position| position.checked_sub(1))
    else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query_as!(
        TurnSizeRow,
        r#"
SELECT message.turn_position AS "turn_position!: i64",
       SUM(message.retained_bytes) AS "retained_bytes!: i64"
FROM session_message AS message
JOIN session_turn AS turn
  ON turn.session_id = message.session_id
 AND turn.turn_position = message.turn_position
WHERE message.session_id = ?
  AND message.turn_position <= ?
  AND turn.status = 'completed'
GROUP BY message.turn_position
ORDER BY message.turn_position DESC
LIMIT ?
"#,
        session_id,
        inclusive_end,
        TURN_SIZE_PAGE_SIZE
    )
    .fetch_all(&mut *connection)
    .await
    .session_context("load persistent session history")?;

    Ok(rows
        .into_iter()
        .map(|row| (row.turn_position, row.retained_bytes))
        .collect())
}

fn decode_max_history_bytes(id: &str, max_history_bytes: i64) -> Result<usize, SessionError> {
    usize::try_from(max_history_bytes).map_err(|_| SessionError::InvalidData {
        reason: format!("session `{id}` has an invalid history byte limit"),
    })
}

async fn next_turn_position(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
) -> Result<i64, SessionError> {
    sqlx::query_scalar::<_, i64>(
        r"
SELECT COALESCE(MAX(turn_position), -1) + 1
FROM session_turn
WHERE session_id = ?
",
    )
    .bind(session_id)
    .fetch_one(&mut **transaction)
    .await
    .session_context("append persistent session turn")
}

async fn latest_completed_turn(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
) -> Result<Option<i64>, SessionError> {
    sqlx::query_scalar::<_, Option<i64>>(
        r"
SELECT MAX(turn_position)
FROM session_turn
WHERE session_id = ? AND status = 'completed'
",
    )
    .bind(session_id)
    .fetch_one(&mut **transaction)
    .await
    .session_context("load persistent session history position")
}

async fn insert_message(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
    turn_position: i64,
    message_position: i64,
    message: &EncodedMessage,
    now: i64,
    operation: &'static str,
) -> Result<(), SessionError> {
    sqlx::query(
        r"
INSERT INTO session_message (
    session_id, turn_position, message_position, kind, payload, retained_bytes, created_at
)
VALUES (?, ?, ?, ?, ?, ?, ?)
",
    )
    .bind(session_id)
    .bind(turn_position)
    .bind(message_position)
    .bind(message.kind)
    .bind(&message.payload)
    .bind(message.retained_bytes)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .session_context(operation)?;

    Ok(())
}

async fn recover_stale_turns(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
    now: i64,
) -> Result<(), SessionError> {
    let result = sqlx::query(
        r"
UPDATE session_turn
SET status = 'interrupted', error_type = 'interrupted', lease_expires_at = NULL, updated_at = ?
WHERE session_id = ?
  AND status IN ('pending', 'running')
  AND lease_expires_at <= ?
",
    )
    .bind(now)
    .bind(session_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .session_context("recover stale persistent session turns")?;
    if result.rows_affected() > 0 {
        sqlx::query(
            r"
UPDATE session
SET provider_session_id = NULL, updated_at = ?
WHERE id = ?
",
        )
        .bind(now)
        .bind(session_id)
        .execute(&mut **transaction)
        .await
        .session_context("recover stale persistent session turns")?;
    }

    Ok(())
}

async fn renew_owned_turn(
    pool: &SqlitePool,
    owner: &TurnOwner,
    now: i64,
) -> Result<bool, sqlx::Error> {
    let lease_expires_at = now.saturating_add(TURN_LEASE_SECONDS);
    let result = sqlx::query(
        r"
UPDATE session_turn
SET lease_expires_at = ?, updated_at = ?
WHERE session_id = ?
  AND turn_position = ?
  AND owner_token = ?
  AND status = 'running'
",
    )
    .bind(lease_expires_at)
    .bind(now)
    .bind(&owner.session_id)
    .bind(owner.turn_position)
    .bind(&owner.token)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}

async fn interrupt_owned_turn(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    owner: &TurnOwner,
    now: i64,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query(
        r"
UPDATE session_turn
SET status = 'interrupted', error_type = ?, lease_expires_at = NULL, updated_at = ?
WHERE session_id = ?
  AND turn_position = ?
  AND owner_token = ?
  AND status IN ('pending', 'running')
",
    )
    .bind(owner.interruption_error_type)
    .bind(now)
    .bind(&owner.session_id)
    .bind(owner.turn_position)
    .bind(&owner.token)
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() > 0 {
        sqlx::query(
            r"
UPDATE session
SET provider_session_id = NULL, updated_at = ?
WHERE id = ?
",
        )
        .bind(now)
        .bind(&owner.session_id)
        .execute(&mut **transaction)
        .await?;
    }

    Ok(())
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
}

fn serialize_payload<T: Serialize>(payload: &T) -> Result<String, SessionError> {
    serde_json::to_string(payload).map_err(|error| invalid_json(&error))
}

fn deserialize_payload<'a, T: Deserialize<'a>>(payload: &'a str) -> Result<T, SessionError> {
    serde_json::from_str(payload).map_err(|error| invalid_json(&error))
}

fn invalid_json(error: &serde_json::Error) -> SessionError {
    SessionError::InvalidData {
        reason: format!("invalid persistent message JSON: {error}"),
    }
}

fn system_timestamp_source() -> Arc<dyn TimestampSource> {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or_default()
    })
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
