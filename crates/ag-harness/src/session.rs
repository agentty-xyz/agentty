//! Durable session ownership and SQLite transactional persistence.

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
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
use tokio::time::Instant;

use crate::model::{ModelMessage, ModelMetadata};
use crate::store::SessionStore;
use crate::tool::{ReadArguments, ToolCall, WriteArguments};
use crate::turn_options_snapshot::{StoredTurnOptions, StoredTurnOptionsError};
use crate::write_journal::{WriteRecord, WriteStatus, content_hash};
use crate::{
    ExecutionIdentity, HostRequest, HostTurnAcquisition, HostTurnRecord, HostTurnStatus,
    OutputSchema, OutputSchemaError, TurnError, TurnOptions, TurnOutcome,
};

pub(crate) const TURN_LEASE_SECONDS: i64 = 300;
pub(crate) const TURN_LEASE_RENEWAL_INTERVAL_SECONDS: u64 = 100;

const DB_POOL_MAX_CONNECTIONS: u32 = 4;
const DB_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const TURN_SIZE_PAGE_SIZE: i64 = 64;

struct WriteRecordRow {
    call_id: String,
    expected_hash: Option<String>,
    id: i64,
    path: String,
    repository_root: Vec<u8>,
    resulting_hash: String,
    status: String,
    turn_position: i64,
}

impl WriteRecordRow {
    async fn load(pool: &SqlitePool, session_id: &str) -> Result<Vec<WriteRecord>, SessionError> {
        let rows = sqlx::query_as!(
            WriteRecordRow,
            r#"
SELECT w.id AS "id!", w.call_id, w.expected_hash, w.path, w.repository_root, w.resulting_hash,
       w.status, w.turn_position
FROM session_write w
WHERE w.session_id = ?
ORDER BY w.turn_position, w.id
"#,
            session_id
        )
        .fetch_all(pool)
        .await
        .map_err(|source| SessionError::QueryContext {
            operation: "load persistent writes",
            source,
        })?;

        rows.into_iter().map(Self::into_record).collect()
    }

    fn into_record(self) -> Result<WriteRecord, SessionError> {
        let repository_root = PathBuf::from(OsString::from_vec(self.repository_root));
        let status = match self.status.as_str() {
            "pending" => WriteStatus::Pending,
            "applied" => WriteStatus::Applied,
            "failed" => WriteStatus::Failed,
            _ => {
                return Err(SessionError::InvalidData {
                    reason: "invalid persistent write status".to_string(),
                });
            }
        };

        Ok(WriteRecord {
            call_id: self.call_id,
            expected_hash: self.expected_hash,
            id: self.id,
            path: self.path,
            repository_root,
            resulting_hash: self.resulting_hash,
            status,
            turn_position: self.turn_position,
        })
    }
}

struct HostTurnRow {
    error_type: Option<String>,
    host_request: Option<String>,
    status: String,
    terminal_outcome: Option<String>,
    turn_position: i64,
}

enum Reservation {
    Acquired(TurnGuard),
    Recorded(HostTurnRecord),
    Retry,
}

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
    registration_key: Option<String>,
    registration_revision: Option<String>,
    system_prompt: Option<String>,
    turn_options: Option<String>,
}

#[derive(Eq, PartialEq)]
struct TurnConfigurationRow {
    max_history_bytes: i64,
    provider_session_id: Option<String>,
    turn_options: Option<String>,
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
pub struct NewSession {
    id: String,
    registration_identity: Option<ExecutionIdentity>,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl NewSession {
    /// Creates a persistent-session configuration.
    pub fn new(id: impl Into<String>, schema: OutputSchema) -> Self {
        Self {
            id: id.into(),
            registration_identity: None,
            schema,
            system_prompt: None,
        }
    }

    /// Sets the system prompt retained across future resumes.
    #[must_use]
    pub fn with_optional_system_prompt(mut self, system_prompt: Option<String>) -> Self {
        self.system_prompt = system_prompt;

        self
    }

    /// Sets the immutable model registration captured when the session is
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

    /// Returns the structured-output schema retained by the session.
    pub fn schema(&self) -> &OutputSchema {
        &self.schema
    }

    /// Returns the optional session system prompt.
    pub fn system_prompt(&self) -> Option<&str> {
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

/// Stable backing-store identity for process-local admission and cleanup.
/// Independent handles for the same backing store must use equal identities.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StoreIdentity(StoreIdentityKind);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum StoreIdentityKind {
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

    async fn for_path(path: &Path) -> Result<Self, std::io::Error> {
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

/// Observes reservation commit submission and acknowledgement boundaries.
#[async_trait]
trait ReservationObserver: Send + Sync {
    async fn committing(&self) {}
    async fn committed(&self);
}

#[async_trait]
impl ReservationObserver for () {
    async fn committed(&self) {}
}

/// SQLite database used by persistent harness sessions.
#[derive(Clone)]
pub struct Database {
    abandoned_turns: Arc<AbandonedTurnRegistry>,
    identity: StoreIdentity,
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
    pub async fn open(path: &Path) -> Result<Self, SessionError> {
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
        // SQLite temporary databases belong to one connection. Additional
        // pooled connections or eviction would lose their schema and history.
        let pool_options = SqlitePoolOptions::new();
        let pool_options = if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            pool_options
                .max_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
                .test_before_acquire(false)
        } else {
            pool_options.max_connections(DB_POOL_MAX_CONNECTIONS)
        };
        let pool = pool_options
            .connect_with(options)
            .await
            .session_context("open persistent session database")?;

        sqlx::migrate!("./migrations").run(&pool).await?;
        let identity = StoreIdentity::for_path(path).await?;

        Ok(Self {
            abandoned_turns: shared_abandoned_turn_registry(),
            identity,
            pool,
            reservation_observer: Arc::new(()),
            timestamp_source,
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

    async fn load_turn_acquisition(
        &self,
        session_id: &str,
    ) -> Result<TurnAcquisition, SessionError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("load persistent session turn acquisition")?;
        let configuration = load_turn_configuration(&mut transaction, session_id).await?;
        let max_history_bytes =
            decode_max_history_bytes(session_id, configuration.max_history_bytes)?;
        let turn_position = next_turn_position(&mut transaction, session_id).await?;
        let latest_completed_turn = latest_completed_turn(&mut transaction, session_id).await?;
        let turns = load_turns_from(&mut transaction, session_id, max_history_bytes).await?;
        transaction
            .commit()
            .await
            .session_context("load persistent session turn acquisition")?;

        Ok(TurnAcquisition {
            configuration,
            latest_completed_turn,
            turn_position,
            turns,
        })
    }

    async fn acquire(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        prompt: &str,
        options: &TurnOptions,
        request: Option<&HostRequest>,
    ) -> Result<HostTurnAcquisition, SessionError> {
        if store.identity() != self.identity() {
            return Err(SessionError::InvalidData {
                reason: "acquisition store has a different backing identity".to_string(),
            });
        }
        let message = EncodedMessage::from_message(&ModelMessage::User(prompt.to_string()))?;

        loop {
            let acquisition = self.load_turn_acquisition(session_id).await?;
            let previous_options = acquisition
                .configuration
                .turn_options
                .as_deref()
                .map(StoredTurnOptions::decode)
                .transpose()?;
            let compatible = previous_options
                .as_ref()
                .is_some_and(|previous| previous.continuation_compatible(options));
            match self
                .reserve_turn(
                    Arc::clone(&store),
                    session_id,
                    &message,
                    &acquisition,
                    options,
                    request,
                )
                .await?
            {
                Reservation::Acquired(guard) => {
                    return Ok(HostTurnAcquisition::Acquired(AcquiredTurn {
                        guard,
                        provider_session_id: acquisition
                            .configuration
                            .provider_session_id
                            .filter(|_| compatible),
                        turns: acquisition.turns,
                    }));
                }
                Reservation::Recorded(record) => return Ok(HostTurnAcquisition::Recorded(record)),
                Reservation::Retry => {}
            }
        }
    }

    async fn complete(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
        outcome: Option<&TurnOutcome>,
    ) -> Result<(), SessionError> {
        let terminal_outcome = outcome
            .map(|outcome| serde_json::json!({"version": 1, "outcome": outcome}).to_string());
        let encoded_messages = messages
            .iter()
            .map(EncodedMessage::from_message)
            .collect::<Result<Vec<_>, _>>()?;
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .session_context("complete persistent session turn")?;
        self.validate_owner(owner)?;
        let session_id = &owner.session_id;
        let turn_position = owner.turn_position;
        let now = self.timestamp_source.now_timestamp_seconds();
        let result = sqlx::query!(
            r"
UPDATE session_turn
SET status = 'completed', error_type = NULL, lease_expires_at = NULL, updated_at = ?, terminal_outcome = ?
WHERE session_id = ? AND turn_position = ? AND status = 'running'
  AND owner_token = ? AND lease_expires_at > ?
  AND (host_id IS NULL OR ? IS NOT NULL)
",
            now,
            terminal_outcome,
            session_id,
            turn_position,
            owner.token,
            now,
            terminal_outcome
        )
        .execute(&mut *transaction)
        .await
        .session_context("complete persistent session turn")?;
        if result.rows_affected() == 0 {
            return Err(owner.lost());
        }
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

    async fn reserve_turn(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        message: &EncodedMessage,
        acquisition: &TurnAcquisition,
        options: &TurnOptions,
        request: Option<&HostRequest>,
    ) -> Result<Reservation, SessionError> {
        let operation = "reserve persistent session turn";
        let recovery_now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .session_context(operation)?;
        recover_stale_turns(&mut transaction, session_id, recovery_now).await?;
        let abandoned_turns = self.abandoned_turns.for_session(&self.identity, session_id);
        for owner in &abandoned_turns {
            interrupt_owned_turn(&mut transaction, owner, recovery_now)
                .await
                .session_context("recover abandoned persistent session turn")?;
        }
        if let Some(request) = request
            && let Some(record) =
                load_request_from(&mut transaction, session_id, request.id()).await?
        {
            record.check_request(request)?;
            transaction.commit().await.session_context(operation)?;

            return Ok(Reservation::Recorded(record));
        }
        let configuration = load_turn_configuration(&mut transaction, session_id).await?;
        let turn_position = next_turn_position(&mut transaction, session_id).await?;
        let latest_completed_turn = latest_completed_turn(&mut transaction, session_id).await?;
        if configuration != acquisition.configuration
            || turn_position != acquisition.turn_position
            || latest_completed_turn != acquisition.latest_completed_turn
        {
            transaction.commit().await.session_context(operation)?;

            return Ok(Reservation::Retry);
        }
        let deadline = lease_deadline();
        let reservation_now = self.timestamp_source.now_timestamp_seconds();
        let lease_expires_at = reservation_now.saturating_add(TURN_LEASE_SECONDS);
        let snapshot = StoredTurnOptions::encode(options);
        let host_id = request.map(HostRequest::id);
        let host_request = request.map(serialize_payload).transpose()?;
        let result = sqlx::query_scalar!(
            r#"
INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at,
    owner_token, turn_options, host_id, host_request
)
VALUES (?, ?, 'running', NULL, ?, ?, ?, randomblob(16), ?, ?, ?)
RETURNING owner_token AS "owner_token!: Vec<u8>"
"#,
            session_id,
            acquisition.turn_position,
            lease_expires_at,
            reservation_now,
            reservation_now,
            snapshot,
            host_id,
            host_request
        )
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
        let guard = TurnGuard::new(store, owner, deadline);
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
        clear_incompatible_continuation(
            &mut transaction,
            session_id,
            &acquisition.configuration,
            options,
        )
        .await?;
        self.commit_reservation(transaction, guard, &abandoned_turns, deadline)
            .await
            .map(Reservation::Acquired)
    }

    async fn commit_reservation(
        &self,
        transaction: Transaction<'static, sqlx::Sqlite>,
        guard: TurnGuard,
        abandoned_turns: &[TurnOwner],
        deadline: Instant,
    ) -> Result<TurnGuard, SessionError> {
        let operation = "reserve persistent session turn";
        let observer = Arc::clone(&self.reservation_observer);
        let mut guard = tokio::spawn(async move {
            observer.committing().await;
            transaction.commit().await.session_context(operation)?;

            Ok::<_, SessionError>(guard)
        })
        .await
        .map_err(|error| SessionError::InvalidData {
            reason: format!("reservation task failed: {error}"),
        })??;
        self.reservation_observer.committed().await;
        if Instant::now() >= deadline {
            return Err(guard.owner.lost());
        }
        self.abandoned_turns.remove(abandoned_turns);
        guard.activate();

        Ok(guard)
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

    fn validate_owner(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        if &owner.database != self.identity() {
            return Err(owner.lost());
        }

        Ok(())
    }
}

#[async_trait]
impl SessionStore for Database {
    fn identity(&self) -> &StoreIdentity {
        &self.identity
    }

    async fn create_session(
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
    id, provider, model, output_schema, system_prompt, max_history_bytes, created_at, updated_at,
    registration_key, registration_revision
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(config.registration_identity().map(ExecutionIdentity::key))
        .bind(
            config
                .registration_identity()
                .map(ExecutionIdentity::revision),
        )
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

    async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError> {
        self.recover_stale_turns(id).await?;
        let row = sqlx::query_as!(
            SessionRow,
            r#"
SELECT provider,
       model,
       registration_key,
       registration_revision,
       output_schema,
       system_prompt,
       max_history_bytes AS "max_history_bytes!: i64",
       provider_session_id,
       (SELECT turn_options FROM session_turn
        WHERE session_id = session.id ORDER BY turn_position DESC LIMIT 1) AS "turn_options?: String"
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
        if let Some(snapshot) = row.turn_options {
            StoredTurnOptions::decode(&snapshot)?;
        }
        let turns = self.load_turns(id, max_history_bytes).await?;
        let registration_identity = match (row.registration_key, row.registration_revision) {
            (None, None) => None,
            (Some(key), Some(revision)) => Some(ExecutionIdentity::new(key, revision)?),
            _ => {
                return Err(SessionError::InvalidData {
                    reason: format!("session `{id}` has incomplete registration identity"),
                });
            }
        };

        Ok(LoadedSession {
            max_history_bytes,
            model: row.model,
            provider: row.provider,
            provider_session_id: row.provider_session_id,
            registration_identity,
            schema,
            system_prompt: row.system_prompt,
            turns,
        })
    }

    async fn begin_turn(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        prompt: &str,
        options: &TurnOptions,
    ) -> Result<AcquiredTurn, SessionError> {
        let HostTurnAcquisition::Acquired(acquired) = self
            .acquire(store, session_id, prompt, options, None)
            .await?
        else {
            return Err(SessionError::HostTurnConflict);
        };

        Ok(acquired)
    }

    async fn begin_request(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        prompt: &str,
        options: &TurnOptions,
        request: &HostRequest,
    ) -> Result<HostTurnAcquisition, SessionError> {
        self.acquire(store, session_id, prompt, options, Some(request))
            .await
    }

    async fn load_request(
        &self,
        session_id: &str,
        host_id: &str,
    ) -> Result<Option<HostTurnRecord>, SessionError> {
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .session_context("recover host request")?;
        load_turn_configuration(&mut transaction, session_id).await?;
        recover_stale_turns(
            &mut transaction,
            session_id,
            self.timestamp_source.now_timestamp_seconds(),
        )
        .await?;
        let record = load_request_from(&mut transaction, session_id, host_id).await?;
        transaction
            .commit()
            .await
            .session_context("recover host request")?;

        Ok(record)
    }

    async fn complete_request(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError> {
        self.complete(owner, messages, continuation, Some(outcome))
            .await
    }

    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
    ) -> Result<(), SessionError> {
        self.complete(owner, messages, continuation, None).await
    }

    async fn fail_turn(&self, owner: &TurnOwner, error: &TurnError) -> Result<(), SessionError> {
        let error_type = format!("{:?}", error.error_type());
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .session_context("fail persistent session turn")?;
        self.validate_owner(owner)?;
        let session_id = &owner.session_id;
        let turn_position = owner.turn_position;
        let now = self.timestamp_source.now_timestamp_seconds();
        let result = sqlx::query!(
            r"
UPDATE session_turn
SET status = 'failed', error_type = ?, lease_expires_at = NULL, updated_at = ?
WHERE session_id = ? AND turn_position = ? AND status = 'running'
  AND owner_token = ? AND lease_expires_at > ?
",
            error_type,
            now,
            session_id,
            turn_position,
            owner.token,
            now
        )
        .execute(&mut *transaction)
        .await
        .session_context("fail persistent session turn")?;
        if result.rows_affected() == 0 {
            return Err(owner.lost());
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

    async fn load_writes(&self, session_id: &str) -> Result<Vec<WriteRecord>, SessionError> {
        WriteRecordRow::load(&self.pool, session_id).await
    }

    async fn write_intent(
        &self,
        owner: &TurnOwner,
        call_id: &str,
        root: &Path,
        path: &str,
        expected: Option<&[u8]>,
        resulting: &[u8],
    ) -> Result<i64, SessionError> {
        self.validate_owner(owner)?;
        let root = root.as_os_str().as_bytes();
        let expected_hash = expected.map(content_hash);
        let resulting_hash = content_hash(resulting);
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|source| SessionError::QueryContext {
                operation: "begin write intent",
                source,
            })?;
        let now = self.timestamp_source.now_timestamp_seconds();
        let row = sqlx::query!(
            r#"
INSERT INTO session_write (
    session_id, turn_position, call_id, repository_root, path,
    expected_hash, resulting_hash, status
)
SELECT session_id, turn_position, ?, ?, ?, ?, ?, 'pending'
FROM session_turn
WHERE session_id = ? AND turn_position = ? AND owner_token = ?
  AND status = 'running' AND lease_expires_at > ?
RETURNING id AS "id!"
"#,
            call_id,
            root,
            path,
            expected_hash,
            resulting_hash,
            owner.session_id,
            owner.turn_position,
            owner.token,
            now
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|source| SessionError::QueryContext {
            operation: "persist write intent",
            source,
        })?
        .ok_or_else(|| SessionError::OwnershipLost {
            id: owner.session_id.clone(),
            turn_position: owner.turn_position,
        })?;

        // `RETURNING` can yield an ID before SQLite reports commit errors.
        transaction
            .commit()
            .await
            .map_err(|source| SessionError::QueryContext {
                operation: "commit write intent",
                source,
            })?;

        Ok(row.id)
    }

    async fn finish_write(
        &self,
        owner: &TurnOwner,
        id: i64,
        applied: bool,
    ) -> Result<(), SessionError> {
        self.validate_owner(owner)?;
        let status = if applied { "applied" } else { "failed" };
        let result = sqlx::query!(
            r"
UPDATE session_write SET status = ?
WHERE id = ? AND session_id = ? AND turn_position = ?
  AND (status = 'pending' OR status = ?)
  AND EXISTS (
    SELECT 1 FROM session_turn
    WHERE session_id = ? AND turn_position = ? AND owner_token = ?
  )
",
            status,
            id,
            owner.session_id,
            owner.turn_position,
            status,
            owner.session_id,
            owner.turn_position,
            owner.token
        )
        .execute(&self.pool)
        .await
        .session_context("persist write outcome")?;
        if result.rows_affected() != 1 {
            return Err(owner.lost());
        }

        Ok(())
    }

    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError> {
        self.validate_owner(owner)?;
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .session_context("renew persistent session turn lease")?;
        let deadline = lease_deadline();
        let now = self.timestamp_source.now_timestamp_seconds();
        if !renew_owned_turn(&mut transaction, owner, now)
            .await
            .session_context("renew persistent session turn lease")?
        {
            return Err(owner.lost());
        }
        transaction
            .commit()
            .await
            .session_context("renew persistent session turn lease")?;

        Ok(deadline)
    }

    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        self.validate_owner(owner)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .session_context("interrupt persistent session turn")?;
        interrupt_owned_turn(
            &mut transaction,
            owner,
            self.timestamp_source.now_timestamp_seconds(),
        )
        .await
        .session_context("interrupt persistent session turn")?;
        transaction
            .commit()
            .await
            .session_context("interrupt persistent session turn")
    }
}

/// Error returned by persistent session operations.
#[derive(Debug, Error)]
pub enum SessionError {
    /// The harness's registration differs from the session's immutable
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
    HostTurnInProgress(Box<crate::HostTurnRecord>),
    /// Failed/interrupted requests are inspectable, never automatically rerun.
    #[error("host turn has stopped; an explicit new attempt requires a new ID")]
    HostTurnStopped(Box<crate::HostTurnRecord>),
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
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    /// The requested session does not exist.
    #[error("persistent session `{id}` does not exist")]
    NotFound {
        /// Missing session identifier.
        id: String,
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
            StoredTurnOptionsError::Json(error) => invalid_json(&error),
            StoredTurnOptionsError::Schema(error) => Self::Schema(error),
        }
    }
}

/// A reserved turn that retains cleanup ownership through commit
/// acknowledgment. Dropping it interrupts only its owner. The Tokio runtime
/// must remain driven until cleanup completes; persistence settlement does not
/// settle filesystem effects.
pub struct AcquiredTurn {
    pub(crate) guard: TurnGuard,
    pub(crate) provider_session_id: Option<String>,
    pub(crate) turns: Vec<Vec<ModelMessage>>,
}

impl AcquiredTurn {
    /// Returns the reservation identity used for backend lifecycle operations.
    pub fn owner(&self) -> &TurnOwner {
        &self.guard.owner
    }

    /// Arms owner-scoped cleanup before submitting the reservation commit.
    /// `store` must be the unchanged handle supplied to `begin_turn`.
    ///
    /// # Errors
    /// Returns an error if the owner identifies another store.
    pub fn new(
        store: Arc<dyn SessionStore>,
        owner: TurnOwner,
        deadline: Instant,
        turns: Vec<Vec<ModelMessage>>,
        provider_session_id: Option<String>,
    ) -> Result<Self, SessionError> {
        if store.identity() != &owner.database {
            return Err(owner.lost());
        }

        Ok(Self {
            guard: TurnGuard::new(store, owner, deadline),
            provider_session_id,
            turns,
        })
    }

    /// Starts ownership monitoring after the reservation is acknowledged.
    /// Renewal is scheduled halfway through the remaining confirmed lease,
    /// capped at 100 seconds, and recalculated after every acknowledgment.
    /// Return this activated value from `begin_turn`.
    ///
    /// # Errors
    /// An expired acknowledgment is interrupted rather than made executable.
    pub fn activate(mut self) -> Result<Self, SessionError> {
        if Instant::now()
            >= *self
                .guard
                .deadline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(self.guard.owner.lost());
        }
        if self.guard.renewal_task.is_none() {
            self.guard.activate();
        }

        Ok(self)
    }
}

/// Session configuration and bounded completed turns returned by a store.
#[derive(Clone)]
pub struct LoadedSession {
    /// Maximum history payload bytes, counted with
    /// `ModelMessage::retained_bytes`.
    pub max_history_bytes: usize,
    /// Stable model name, paired with `provider`, or both absent.
    pub model: Option<String>,
    /// Provider paired with the stored model name.
    pub provider: Option<String>,
    /// Optional provider continuation, cleared on failure or interruption.
    pub provider_session_id: Option<String>,
    /// Immutable registration key/revision, or `None` for direct/legacy
    /// sessions.
    pub registration_identity: Option<ExecutionIdentity>,
    /// Session's default output schema.
    pub schema: OutputSchema,
    /// Session's retained system prompt.
    pub system_prompt: Option<String>,
    /// Recent complete turns, oldest first; never split tool-call/result
    /// groups.
    pub turns: Vec<Vec<ModelMessage>>,
}

struct TurnAcquisition {
    configuration: TurnConfigurationRow,
    latest_completed_turn: Option<i64>,
    turn_position: i64,
    turns: Vec<Vec<ModelMessage>>,
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

    fn lost(&self) -> SessionError {
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

fn lease_deadline() -> Instant {
    // Stored timestamps round down to seconds; never promise the fractional
    // second.
    Instant::now() + Duration::from_secs(TURN_LEASE_SECONDS.unsigned_abs() - 1)
}

#[derive(Default)]
struct AbandonedTurnRegistry {
    owners: Mutex<HashMap<TurnOwner, Option<Arc<dyn SessionStore>>>>,
}

impl AbandonedTurnRegistry {
    fn for_session(&self, database: &StoreIdentity, session_id: &str) -> Vec<TurnOwner> {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .filter(|owner| &owner.database == database && owner.session_id == session_id)
            .cloned()
            .collect()
    }

    fn retain(&self, owner: TurnOwner, store: Arc<dyn SessionStore>) {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(owner, Some(store));
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

pub(crate) async fn recover_abandoned(
    store: &Arc<dyn SessionStore>,
    session_id: &str,
) -> Result<(), SessionError> {
    let registry = shared_abandoned_turn_registry();
    for owner in registry.for_session(store.identity(), session_id) {
        let retained = registry
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&owner)
            .and_then(Clone::clone)
            .unwrap_or_else(|| Arc::clone(store));
        retained.interrupt(&owner).await?;
        registry.remove(std::slice::from_ref(&owner));
    }

    Ok(())
}

pub(crate) async fn recover_abandoned_owner(owner: &TurnOwner) -> Result<(), SessionError> {
    let registry = shared_abandoned_turn_registry();
    let store = registry
        .owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(owner)
        .and_then(Clone::clone);
    if let Some(store) = store {
        store.interrupt(owner).await?;
        registry.remove(std::slice::from_ref(owner));
    }

    Ok(())
}

/// Owned journal access scoped to the turn that acquired it.
#[derive(Clone)]
pub(crate) struct WriteJournal {
    database: Arc<dyn SessionStore>,
    owner: TurnOwner,
}

impl WriteJournal {
    pub(crate) async fn intent(
        &self,
        call_id: &str,
        root: &Path,
        path: &str,
        expected: Option<&[u8]>,
        resulting: &[u8],
    ) -> Result<i64, SessionError> {
        self.database
            .write_intent(&self.owner, call_id, root, path, expected, resulting)
            .await
    }

    pub(crate) async fn finish(&self, id: i64, applied: bool) -> Result<(), SessionError> {
        self.database.finish_write(&self.owner, id, applied).await
    }
}

pub(crate) struct TurnGuard {
    armed: bool,
    database: Arc<dyn SessionStore>,
    deadline: Arc<Mutex<Instant>>,
    finalization: Arc<tokio::sync::Mutex<()>>,
    owner: TurnOwner,
    ownership_failure: Option<oneshot::Receiver<SessionError>>,
    renewal_stop: Option<oneshot::Sender<()>>,
    renewal_task: Option<JoinHandle<()>>,
    runtime: tokio::runtime::Handle,
}

impl TurnGuard {
    pub(crate) fn write_journal(&self) -> WriteJournal {
        WriteJournal {
            database: Arc::clone(&self.database),
            owner: self.owner.clone(),
        }
    }

    /// Retain cleanup responsibility before committing a reservation. Activate
    /// only after the commit is acknowledged within the confirmed deadline.
    pub(crate) fn new(
        database: Arc<dyn SessionStore>,
        owner: TurnOwner,
        deadline: Instant,
    ) -> Self {
        Self {
            armed: true,
            database,
            deadline: Arc::new(Mutex::new(deadline)),
            finalization: Arc::new(tokio::sync::Mutex::new(())),
            owner,
            ownership_failure: None,
            renewal_stop: None,
            renewal_task: None,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    pub(crate) fn activate(&mut self) {
        self.owner.interruption_error_type = "cancelled";
        let owner = self.owner.clone();
        let interval = Duration::from_secs(TURN_LEASE_RENEWAL_INTERVAL_SECONDS);
        let mut confirmed_at = Instant::now();
        let database = Arc::clone(&self.database);
        let deadline = Arc::clone(&self.deadline);
        let finalization = Arc::clone(&self.finalization);
        let (renewal_stop, mut stop_requested) = oneshot::channel();
        let (ownership_failed, ownership_failure) = oneshot::channel();
        self.renewal_stop = Some(renewal_stop);
        self.ownership_failure = Some(ownership_failure);
        self.renewal_task = Some(self.runtime.spawn(async move {
            let monitor = async {
                loop {
                    let confirmed = *deadline
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let renew_at = confirmed_at
                        + interval.min(confirmed.saturating_duration_since(confirmed_at) / 2);
                    let renewal = async {
                        tokio::time::sleep_until(renew_at).await;
                        let _exclusive = finalization.lock().await;
                        let renewed = database.renew(&owner).await?;
                        *deadline
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = renewed;
                        confirmed_at = Instant::now();

                        Ok::<_, SessionError>(())
                    };
                    tokio::select! {
                        biased;
                        () = tokio::time::sleep_until(confirmed) => return owner.lost(),
                        result = renewal => if let Err(error) = result { return error; },
                    }
                }
            };
            tokio::select! {
                error = monitor => { let _ = ownership_failed.send(error); }
                _ = &mut stop_requested => {}
            }
        }));
    }

    pub(crate) async fn ownership_failure(&mut self) -> SessionError {
        if Instant::now()
            >= *self
                .deadline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return self.owner.lost();
        }
        let Some(failure) = self.ownership_failure.as_mut() else {
            return self.owner.lost();
        };
        let result = failure.await.unwrap_or_else(|_| self.owner.lost());
        self.ownership_failure = None;

        result
    }

    /// Renewal and finalization cannot race their acknowledgements. Waiting
    /// for a stalled renewal remains bounded by its last confirmed deadline.
    pub(crate) async fn complete(
        &mut self,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
    ) -> Result<(), SessionError> {
        let database = Arc::clone(&self.database);
        let owner = self.owner.clone();
        self.finalize(database.complete_turn(&owner, messages, provider_session_id))
            .await
    }

    pub(crate) async fn complete_request(
        &mut self,
        messages: &[ModelMessage],
        continuation: Option<&str>,
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError> {
        let database = Arc::clone(&self.database);
        let owner = self.owner.clone();
        self.finalize(database.complete_request(&owner, messages, continuation, outcome))
            .await
    }

    pub(crate) async fn fail(&mut self, error: &TurnError) -> Result<(), SessionError> {
        let database = Arc::clone(&self.database);
        let owner = self.owner.clone();
        self.finalize(database.fail_turn(&owner, error)).await
    }

    async fn finalize(
        &mut self,
        persistence: impl Future<Output = Result<(), SessionError>>,
    ) -> Result<(), SessionError> {
        let finalization = Arc::clone(&self.finalization);
        let _exclusive = tokio::select! {
            biased;
            error = self.ownership_failure() => return Err(error),
            exclusive = finalization.lock() => exclusive,
        };
        self.stop_renewal();
        let deadline = *self
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if Instant::now() >= deadline {
            return Err(self.owner.lost());
        }
        let result = tokio::time::timeout_at(deadline, persistence)
            .await
            .unwrap_or_else(|_| Err(self.owner.lost()));
        if result.is_ok() {
            self.disarm();
        }

        result
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
        if let Some(task) = self.renewal_task.take() {
            task.abort();
        }
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.stop_renewal();
        if !self.armed {
            return;
        }
        let owner = self.owner.clone();
        let registry = shared_abandoned_turn_registry();
        registry.retain(owner.clone(), Arc::clone(&self.database));
        let database = Arc::clone(&self.database);
        std::mem::drop(self.runtime.spawn(async move {
            if database.interrupt(&owner).await.is_ok() {
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
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
}

async fn clear_incompatible_continuation(
    connection: &mut SqliteConnection,
    session_id: &str,
    configuration: &TurnConfigurationRow,
    options: &TurnOptions,
) -> Result<(), SessionError> {
    let continuation_compatible = configuration
        .turn_options
        .as_deref()
        .map(StoredTurnOptions::decode)
        .transpose()?
        .is_some_and(|previous| previous.continuation_compatible(options));
    if !continuation_compatible {
        sqlx::query!(
            "UPDATE session SET provider_session_id = NULL WHERE id = ?",
            session_id
        )
        .execute(connection)
        .await
        .session_context("reserve persistent session turn")?;
    }
    Ok(())
}

async fn load_request_from(
    connection: &mut SqliteConnection,
    session_id: &str,
    host_id: &str,
) -> Result<Option<HostTurnRecord>, SessionError> {
    let row = sqlx::query_as!(HostTurnRow,
        r#"SELECT error_type, host_request, status, terminal_outcome, turn_position AS "turn_position!: i64"
           FROM session_turn WHERE session_id = ? AND host_id = ?"#, session_id, host_id)
        .fetch_optional(&mut *connection).await.session_context("load host request")?;
    let Some(row) = row else {
        return Ok(None);
    };
    let request = deserialize_payload(row.host_request.as_deref().unwrap_or(""))?;
    let status = match row.status.as_str() {
        "pending" | "running" => HostTurnStatus::InProgress,
        "completed" => {
            let stored: Value = deserialize_payload(row.terminal_outcome.as_deref().unwrap_or(""))?;
            if stored["version"] != 1 {
                return Err(SessionError::InvalidData {
                    reason: "unsupported host outcome version".into(),
                });
            }
            HostTurnStatus::Completed(
                serde_json::from_value(stored["outcome"].clone())
                    .map_err(|error| invalid_json(&error))?,
            )
        }
        "failed" => HostTurnStatus::Failed {
            error_type: row.error_type.unwrap_or_default(),
        },
        "interrupted" => HostTurnStatus::Interrupted {
            error_type: row.error_type.unwrap_or_default(),
        },
        _ => {
            return Err(SessionError::InvalidData {
                reason: "invalid host turn status".into(),
            });
        }
    };
    let position = row.turn_position;
    let rows = sqlx::query_as!(WriteRecordRow,
        r#"SELECT id AS "id!", call_id, expected_hash, path, repository_root, resulting_hash,
                  status, turn_position FROM session_write WHERE session_id = ? AND turn_position = ? ORDER BY id"#,
        session_id, position).fetch_all(&mut *connection).await.session_context("load host writes")?;
    let writes = rows
        .into_iter()
        .map(WriteRecordRow::into_record)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Some(HostTurnRecord {
        request,
        status,
        turn_position: position,
        writes,
    }))
}

async fn load_turn_configuration(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> Result<TurnConfigurationRow, SessionError> {
    sqlx::query_as!(
        TurnConfigurationRow,
        r#"
SELECT max_history_bytes AS "max_history_bytes!: i64", provider_session_id,
       (SELECT turn_options FROM session_turn
        WHERE session_id = session.id AND status = 'completed'
        ORDER BY turn_position DESC LIMIT 1) AS "turn_options?: String"
FROM session WHERE id = ?
"#,
        session_id
    )
    .fetch_optional(connection)
    .await
    .session_context("load persistent session turn acquisition")?
    .ok_or_else(|| SessionError::NotFound {
        id: session_id.to_string(),
    })
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
    connection: &mut SqliteConnection,
    owner: &TurnOwner,
    now: i64,
) -> Result<bool, sqlx::Error> {
    let lease_expires_at = now.saturating_add(TURN_LEASE_SECONDS);
    let result = sqlx::query!(
        r"
UPDATE session_turn
SET lease_expires_at = ?, updated_at = ?
WHERE session_id = ?
  AND turn_position = ?
  AND owner_token = ?
  AND status = 'running'
  AND lease_expires_at > ?
",
        lease_expires_at,
        now,
        owner.session_id,
        owner.turn_position,
        owner.token,
        now
    )
    .execute(connection)
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
