//! Completed-turn persistence for resumable harness chats.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{SqlitePool, Transaction};
use thiserror::Error;

use crate::model::{ModelMessage, ModelMetadata};
use crate::tool::{ReadArguments, ToolCall, WriteArguments};
use crate::{OutputSchema, OutputSchemaError, TurnError};

const DB_POOL_MAX_CONNECTIONS: u32 = 4;
const DB_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const TURN_SIZE_PAGE_SIZE: i64 = 64;

/// Configuration stored when creating a persistent chat session.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    id: String,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl SessionConfig {
    /// Creates a persistent-session configuration.
    pub fn new(id: impl Into<String>, schema: OutputSchema) -> Self {
        Self {
            id: id.into(),
            schema,
            system_prompt: None,
        }
    }

    /// Adds a system prompt that is restored with the session.
    #[must_use]
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());

        self
    }

    /// Returns the stable application-provided session identifier.
    pub fn id(&self) -> &str {
        &self.id
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
pub trait TimestampSource: Send + Sync {
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

/// SQLite database used by persistent harness sessions.
#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
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
    pub async fn open_with_timestamp_source(
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

        Ok(Self {
            pool,
            timestamp_source,
        })
    }

    /// Opens an isolated in-memory SQLite database and runs migrations.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened or a migration
    /// fails.
    pub async fn open_in_memory() -> Result<Self, SessionError> {
        Self::open_in_memory_with_timestamp_source(system_timestamp_source()).await
    }

    /// Opens an in-memory database with an injected timestamp source.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened or a migration
    /// fails.
    pub async fn open_in_memory_with_timestamp_source(
        timestamp_source: Arc<dyn TimestampSource>,
    ) -> Result<Self, SessionError> {
        let options = connect_options(Path::new(":memory:"));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .session_context("open in-memory persistent session database")?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(Self {
            pool,
            timestamp_source,
        })
    }

    pub(crate) async fn create_session(
        &self,
        config: &SessionConfig,
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
        type SessionRow = (Option<String>, Option<String>, String, Option<String>, i64);
        let row = sqlx::query_as::<_, SessionRow>(
            r"
SELECT provider, model, output_schema, system_prompt, max_history_bytes
FROM session
WHERE id = ?
",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .session_context("load persistent session")?
        .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        let (provider, model, output_schema, system_prompt, max_history_bytes) = row;
        let max_history_bytes =
            usize::try_from(max_history_bytes).map_err(|_| SessionError::InvalidData {
                reason: format!("session `{id}` has an invalid history byte limit"),
            })?;
        let output_schema = serde_json::from_str::<Value>(&output_schema).map_err(|error| {
            SessionError::InvalidData {
                reason: format!("session `{id}` has invalid output-schema JSON: {error}"),
            }
        })?;
        let schema = OutputSchema::new(output_schema)?;
        let turns = self.load_turns(id, max_history_bytes).await?;

        Ok(LoadedSession {
            max_history_bytes,
            model,
            provider,
            schema,
            system_prompt,
            turns,
        })
    }

    pub(crate) async fn append_turn(
        &self,
        session_id: &str,
        messages: &[ModelMessage],
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
            .session_context("append persistent session turn")?;
        let result = sqlx::query(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
        )
        .bind(now)
        .bind(session_id)
        .execute(&mut *transaction)
        .await
        .session_context("append persistent session turn")?;
        if result.rows_affected() == 0 {
            return Err(SessionError::NotFound {
                id: session_id.to_string(),
            });
        }
        let turn_position = next_turn_position(&mut transaction, session_id).await?;

        for (message_position, message) in encoded_messages.iter().enumerate() {
            let message_position = i64::try_from(message_position).unwrap_or(i64::MAX);
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
            .execute(&mut *transaction)
            .await
            .session_context("append persistent session turn")?;
        }

        transaction
            .commit()
            .await
            .session_context("append persistent session turn")?;

        Ok(())
    }

    async fn load_turns(
        &self,
        session_id: &str,
        max_history_bytes: usize,
    ) -> Result<Vec<Vec<ModelMessage>>, SessionError> {
        let mut retained_bytes = 0_usize;
        let mut oldest_turn = None;
        let mut before_turn = None;

        'pages: loop {
            let turn_sizes = load_turn_size_page(&self.pool, session_id, before_turn).await?;
            if turn_sizes.is_empty() {
                break;
            }
            let page_is_full =
                turn_sizes.len() == usize::try_from(TURN_SIZE_PAGE_SIZE).unwrap_or(usize::MAX);

            for (turn_position, turn_bytes) in turn_sizes {
                let turn_bytes =
                    usize::try_from(turn_bytes).map_err(|_| SessionError::InvalidData {
                        reason: format!(
                            "session `{session_id}` has an invalid retained byte count"
                        ),
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

        let Some(oldest_turn) = oldest_turn else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            r"
SELECT turn_position, kind, payload
FROM session_message
WHERE session_id = ? AND turn_position >= ?
ORDER BY turn_position, message_position
",
        )
        .bind(session_id)
        .bind(oldest_turn)
        .fetch_all(&self.pool)
        .await
        .session_context("load persistent session history")?;
        let mut turns = Vec::<Vec<ModelMessage>>::new();
        let mut current_position = None;

        for (turn_position, kind, payload) in rows {
            if current_position != Some(turn_position) {
                turns.push(Vec::new());
                current_position = Some(turn_position);
            }
            let message = EncodedMessage::into_message(&kind, &payload)?;
            if let Some(turn) = turns.last_mut() {
                turn.push(message);
            }
        }

        Ok(turns)
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
    /// The model turn failed before it could be persisted.
    #[error(transparent)]
    Turn(#[from] TurnError),
}

pub(crate) struct LoadedSession {
    pub(crate) max_history_bytes: usize,
    pub(crate) model: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) schema: OutputSchema,
    pub(crate) system_prompt: Option<String>,
    pub(crate) turns: Vec<Vec<ModelMessage>>,
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

async fn load_turn_size_page(
    pool: &SqlitePool,
    session_id: &str,
    before_turn: Option<i64>,
) -> Result<Vec<(i64, i64)>, SessionError> {
    let query = if let Some(before_turn) = before_turn {
        sqlx::query_as::<_, (i64, i64)>(
            r"
SELECT turn_position, SUM(retained_bytes)
FROM session_message
WHERE session_id = ? AND turn_position < ?
GROUP BY turn_position
ORDER BY turn_position DESC
LIMIT ?
",
        )
        .bind(session_id)
        .bind(before_turn)
    } else {
        sqlx::query_as::<_, (i64, i64)>(
            r"
SELECT turn_position, SUM(retained_bytes)
FROM session_message
WHERE session_id = ?
GROUP BY turn_position
ORDER BY turn_position DESC
LIMIT ?
",
        )
        .bind(session_id)
    };

    query
        .bind(TURN_SIZE_PAGE_SIZE)
        .fetch_all(pool)
        .await
        .session_context("load persistent session history")
}

async fn next_turn_position(
    transaction: &mut Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
) -> Result<i64, SessionError> {
    sqlx::query_scalar::<_, i64>(
        r"
SELECT COALESCE(MAX(turn_position), -1) + 1
FROM session_message
WHERE session_id = ?
",
    )
    .bind(session_id)
    .fetch_one(&mut **transaction)
    .await
    .session_context("append persistent session turn")
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
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::model::{MockModel, ModelResponse};

    fn schema() -> OutputSchema {
        OutputSchema::new(json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn model() -> MockModel {
        let mut model = MockModel::new();
        model.expect_metadata().return_const(None);

        model
    }

    fn metadata_model(provider: &'static str, model_name: &str) -> MockModel {
        let mut model = MockModel::new();
        let metadata = ModelMetadata::new(provider, model_name).expect("metadata should be valid");
        model.expect_metadata().return_const(Some(metadata));

        model
    }

    fn read_call(id: &str) -> ToolCall {
        let arguments = serde_json::from_value::<ReadArguments>(json!({
            "action": "file",
            "path": "Cargo.toml",
            "limit": 1
        }))
        .expect("read arguments should be valid");

        ToolCall::read(
            id.to_string(),
            arguments,
            Some("inspect manifest".to_string()),
        )
    }

    fn write_call(id: &str) -> ToolCall {
        let arguments = serde_json::from_value::<WriteArguments>(json!({
            "path": "src/lib.rs",
            "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
        }))
        .expect("write arguments should be valid");

        ToolCall::write(id.to_string(), arguments, None)
    }

    fn turn(prompt: &str, answer: &str) -> Vec<ModelMessage> {
        vec![
            ModelMessage::User(prompt.to_string()),
            ModelMessage::Assistant(format!(r#"{{"summary":"{answer}"}}"#)),
        ]
    }

    #[test]
    fn session_config_exposes_values_and_system_prompt() {
        // Arrange
        let schema = schema();

        // Act
        let config = SessionConfig::new("session-a", schema.clone())
            .with_system_prompt("persistent instructions");

        // Assert
        assert_eq!(config.id(), "session-a");
        assert_eq!(config.schema(), &schema);
        assert_eq!(config.system_prompt(), Some("persistent instructions"));
    }

    #[test]
    fn timestamp_source_closure_returns_injected_value() {
        // Arrange
        let timestamp_source = || 123;

        // Act
        let timestamp = timestamp_source.now_timestamp_seconds();

        // Assert
        assert_eq!(timestamp, 123);
    }

    #[tokio::test]
    async fn on_disk_database_creates_parent_and_applies_connection_policy() {
        // Arrange
        let temp_dir = tempdir().expect("temp directory should be created");
        let database_path = temp_dir.path().join("nested/harness.db");
        let timestamp_source: Arc<dyn TimestampSource> = Arc::new(|| 123);

        // Act
        let database = Database::open_with_timestamp_source(&database_path, timestamp_source)
            .await
            .expect("database should open");
        let journal_mode = sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
            .fetch_one(&database.pool)
            .await
            .expect("journal mode should load");
        let foreign_keys = sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
            .fetch_one(&database.pool)
            .await
            .expect("foreign key setting should load");
        let synchronous = sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
            .fetch_one(&database.pool)
            .await
            .expect("synchronous setting should load");

        // Assert
        assert!(database_path.exists());
        assert_eq!(journal_mode, "wal");
        assert_eq!(foreign_keys, 1);
        assert_eq!(synchronous, 1);
    }

    #[tokio::test]
    async fn on_disk_database_supports_sqlite_temporary_paths_without_a_parent() {
        // Arrange
        let database_path = Path::new("");

        // Act
        let result = Database::open(database_path).await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn default_database_constructors_use_system_timestamps() {
        // Arrange
        let temp_dir = tempdir().expect("temp directory should be created");
        let database_path = temp_dir.path().join("harness.db");

        // Act
        let on_disk = Database::open(&database_path)
            .await
            .expect("on-disk database should open");
        let in_memory = Database::open_in_memory()
            .await
            .expect("in-memory database should open");

        // Assert
        assert!(on_disk.timestamp_source.now_timestamp_seconds() > 0);
        assert!(in_memory.timestamp_source.now_timestamp_seconds() > 0);
    }

    #[tokio::test]
    async fn database_round_trips_every_persistent_message_kind() {
        // Arrange
        let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 456))
            .await
            .expect("database should open");
        let config =
            SessionConfig::new("session-a", schema()).with_system_prompt("persistent instructions");
        let metadata = ModelMetadata::new("provider", "model").expect("metadata should be valid");
        database
            .create_session(&config, Some(metadata), 100_000)
            .await
            .expect("session should be created");
        let messages = vec![
            ModelMessage::User("inspect and edit".to_string()),
            ModelMessage::AssistantToolCall(read_call("read-one")),
            ModelMessage::ToolResult {
                call_id: "read-one".to_string(),
                content: "manifest".to_string(),
                name: "read".to_string(),
            },
            ModelMessage::AssistantToolCalls(vec![read_call("read-two"), write_call("write-one")]),
            ModelMessage::ToolResult {
                call_id: "read-two".to_string(),
                content: "source".to_string(),
                name: "read".to_string(),
            },
            ModelMessage::ToolResult {
                call_id: "write-one".to_string(),
                content: "written".to_string(),
                name: "write".to_string(),
            },
            ModelMessage::Assistant(r#"{"summary":"done"}"#.to_string()),
        ];

        // Act
        database
            .append_turn("session-a", &messages)
            .await
            .expect("turn should be appended");
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");
        let timestamps = sqlx::query_as::<_, (i64, i64, i64)>(
            r"
SELECT session.created_at, session.updated_at, session_message.created_at
FROM session
INNER JOIN session_message ON session_message.session_id = session.id
WHERE session.id = ?
LIMIT 1
",
        )
        .bind("session-a")
        .fetch_one(&database.pool)
        .await
        .expect("timestamps should load");

        // Assert
        assert_eq!(loaded.provider.as_deref(), Some("provider"));
        assert_eq!(loaded.model.as_deref(), Some("model"));
        assert_eq!(loaded.schema, schema());
        assert_eq!(
            loaded.system_prompt.as_deref(),
            Some("persistent instructions")
        );
        assert_eq!(loaded.turns, vec![messages]);
        assert_eq!(timestamps, (456, 456, 456));
    }

    #[tokio::test]
    async fn database_loads_only_newest_complete_turns_within_budget() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let older_turn = turn("older question", "older answer");
        let latest_turn = turn("latest question", "latest answer");
        let latest_bytes = latest_turn.iter().map(ModelMessage::retained_bytes).sum();
        database
            .create_session(
                &SessionConfig::new("session-a", schema()),
                None,
                latest_bytes,
            )
            .await
            .expect("session should be created");
        database
            .append_turn("session-a", &older_turn)
            .await
            .expect("older turn should be appended");
        database
            .append_turn("session-a", &latest_turn)
            .await
            .expect("latest turn should be appended");

        // Act
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");

        // Assert
        assert_eq!(loaded.turns, vec![latest_turn]);
    }

    #[tokio::test]
    async fn database_paginates_turn_sizes_until_the_history_budget_is_filled() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let turns = (0..70)
            .map(|position| {
                turn(
                    &format!("question {position}"),
                    &format!("answer {position}"),
                )
            })
            .collect::<Vec<_>>();
        let expected_turns = turns[5..].to_vec();
        let max_history_bytes = expected_turns
            .iter()
            .flatten()
            .map(ModelMessage::retained_bytes)
            .sum();
        database
            .create_session(
                &SessionConfig::new("session-a", schema()),
                None,
                max_history_bytes,
            )
            .await
            .expect("session should be created");
        for messages in &turns {
            database
                .append_turn("session-a", messages)
                .await
                .expect("turn should be appended");
        }

        // Act
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");

        // Assert
        assert_eq!(loaded.turns, expected_turns);
    }

    #[tokio::test]
    async fn database_excludes_a_newest_turn_larger_than_the_budget() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&SessionConfig::new("session-a", schema()), None, 1)
            .await
            .expect("session should be created");
        database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect("turn should be appended");

        // Act
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");

        // Assert
        assert_eq!(loaded.turns, Vec::<Vec<ModelMessage>>::new());
    }

    #[tokio::test]
    async fn append_turn_rolls_back_every_message_when_one_insert_fails() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&SessionConfig::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        sqlx::query(
            r"
CREATE TRIGGER reject_assistant_message
BEFORE INSERT ON session_message
WHEN NEW.kind = 'assistant'
BEGIN
    SELECT RAISE(ABORT, 'assistant rejected');
END
",
        )
        .execute(&database.pool)
        .await
        .expect("trigger should be created");

        // Act
        let error = database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect_err("turn append should fail");
        let message_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM session_message WHERE session_id = ?",
        )
        .bind("session-a")
        .fetch_one(&database.pool)
        .await
        .expect("message count should load");

        // Assert
        assert!(matches!(error, SessionError::QueryContext { .. }));
        assert_eq!(message_count, 0);
    }

    #[tokio::test]
    async fn on_disk_database_serializes_concurrent_appends_before_reading_positions() {
        // Arrange
        let temp_dir = tempdir().expect("temp directory should be created");
        let database_path = temp_dir.path().join("harness.db");
        let next_timestamp = Arc::new(AtomicI64::new(1));
        let timestamp_source: Arc<dyn TimestampSource> = {
            let next_timestamp = Arc::clone(&next_timestamp);

            Arc::new(move || next_timestamp.fetch_add(1, Ordering::SeqCst))
        };
        let database = Database::open_with_timestamp_source(&database_path, timestamp_source)
            .await
            .expect("database should open");
        for session_id in ["session-a", "session-b"] {
            database
                .create_session(&SessionConfig::new(session_id, schema()), None, 100_000)
                .await
                .expect("session should be created");
        }
        sqlx::query(
            r"
CREATE TRIGGER require_session_update_before_message
BEFORE INSERT ON session_message
WHEN (
    SELECT updated_at
    FROM session
    WHERE id = NEW.session_id
) != NEW.created_at
BEGIN
    SELECT RAISE(ABORT, 'session update must acquire the writer lock first');
END
",
        )
        .execute(&database.pool)
        .await
        .expect("ordering trigger should be created");
        let first_turn = turn("first question", "first answer");
        let second_turn = turn("second question", "second answer");

        // Act
        let (first_result, second_result) = tokio::join!(
            database.append_turn("session-a", &first_turn),
            database.append_turn("session-b", &second_turn),
        );

        // Assert
        first_result.expect("first concurrent append should succeed");
        second_result.expect("second concurrent append should succeed");
        let message_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_message")
            .fetch_one(&database.pool)
            .await
            .expect("message count should load");
        assert_eq!(message_count, 4);
    }

    #[tokio::test]
    async fn appending_an_empty_turn_to_a_missing_session_reports_not_found() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");

        // Act
        let error = database
            .append_turn("missing", &[])
            .await
            .expect_err("missing session should fail");

        // Assert
        assert!(matches!(error, SessionError::NotFound { .. }));
    }

    #[tokio::test]
    async fn database_reports_creation_and_loading_errors() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let config = SessionConfig::new("session-a", schema());
        database
            .create_session(&config, None, 100)
            .await
            .expect("session should be created");

        // Act
        let duplicate = database
            .create_session(&config, None, 100)
            .await
            .expect_err("duplicate should fail");
        let empty = database
            .create_session(&SessionConfig::new(" ", schema()), None, 100)
            .await
            .expect_err("empty identifier should fail");
        let oversized_limit = database
            .create_session(&SessionConfig::new("session-b", schema()), None, usize::MAX)
            .await
            .expect_err("oversized limit should fail");
        let missing = database
            .load_session("missing")
            .await
            .err()
            .expect("missing session should fail");

        // Assert
        assert!(matches!(duplicate, SessionError::AlreadyExists { .. }));
        assert!(matches!(empty, SessionError::InvalidData { .. }));
        assert!(matches!(oversized_limit, SessionError::InvalidData { .. }));
        assert!(matches!(missing, SessionError::NotFound { .. }));
    }

    #[tokio::test]
    async fn database_rejects_invalid_persisted_schema_and_message_data() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&SessionConfig::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect("turn should be appended");
        sqlx::query("UPDATE session SET output_schema = 'not-json' WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("schema should be corrupted");

        // Act
        let invalid_schema = database
            .load_session("session-a")
            .await
            .err()
            .expect("invalid schema should fail");
        sqlx::query("UPDATE session SET output_schema = '{}' WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("schema should be repaired");
        sqlx::query("UPDATE session_message SET kind = 'unknown' WHERE session_id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("message kind should be corrupted");
        let invalid_message = database
            .load_session("session-a")
            .await
            .err()
            .expect("invalid message should fail");

        // Assert
        assert!(matches!(invalid_schema, SessionError::InvalidData { .. }));
        assert!(matches!(invalid_message, SessionError::InvalidData { .. }));
    }

    #[tokio::test]
    async fn database_rejects_negative_persisted_byte_counts() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&SessionConfig::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        database
            .append_turn("session-a", &turn("question", "answer"))
            .await
            .expect("turn should be appended");
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&database.pool)
            .await
            .expect("constraints should be disabled for corruption fixture");
        sqlx::query("UPDATE session SET max_history_bytes = -1 WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("history byte limit should be corrupted");

        // Act
        let invalid_limit = database
            .load_session("session-a")
            .await
            .err()
            .expect("negative history byte limit should fail");
        sqlx::query("UPDATE session SET max_history_bytes = 100000 WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("history byte limit should be repaired");
        sqlx::query(
            "UPDATE session_message SET retained_bytes = -1 WHERE session_id = 'session-a'",
        )
        .execute(&database.pool)
        .await
        .expect("retained byte count should be corrupted");
        let invalid_message_size = database
            .load_session("session-a")
            .await
            .err()
            .expect("negative retained byte count should fail");

        // Assert
        assert!(matches!(invalid_limit, SessionError::InvalidData { .. }));
        assert!(matches!(
            invalid_message_size,
            SessionError::InvalidData { .. }
        ));
    }

    #[test]
    fn message_decoder_rejects_invalid_json_arguments_and_unknown_tools() {
        // Arrange
        let invalid_json = "{";
        let invalid_read =
            r#"{"arguments":{"bogus":true},"id":"call","name":"read","reasoning_content":null}"#;
        let unknown_tool = r#"{"arguments":{},"id":"call","name":"bash","reasoning_content":null}"#;

        // Act
        let malformed = EncodedMessage::into_message("user", invalid_json);
        let invalid_arguments = EncodedMessage::into_message("assistant_tool_call", invalid_read);
        let unknown = EncodedMessage::into_message("assistant_tool_call", unknown_tool);
        let system = EncodedMessage::from_message(&ModelMessage::System("system".to_string()));

        // Assert
        assert!(matches!(malformed, Err(SessionError::InvalidData { .. })));
        assert!(matches!(
            invalid_arguments,
            Err(SessionError::InvalidData { .. })
        ));
        assert!(matches!(unknown, Err(SessionError::InvalidData { .. })));
        assert!(matches!(system, Err(SessionError::InvalidData { .. })));
    }

    #[tokio::test]
    async fn persistent_chat_restores_completed_history_and_system_prompt() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |request| {
                let expected = if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    vec![
                        ModelMessage::System("persistent instructions".to_string()),
                        ModelMessage::User("first".to_string()),
                    ]
                } else {
                    vec![
                        ModelMessage::System("persistent instructions".to_string()),
                        ModelMessage::User("first".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                        ModelMessage::User("second".to_string()),
                    ]
                };
                assert_eq!(request.messages(), expected);

                Ok((
                    ModelResponse::Output(json!({
                        "summary": if expected.len() == 2 { "one" } else { "two" }
                    })),
                    None,
                ))
            });
        let harness = crate::Harness::new(model);
        let config =
            SessionConfig::new("session-a", schema()).with_system_prompt("persistent instructions");
        let mut session = harness
            .create_session(&database, config)
            .await
            .expect("session should be created");

        // Act
        let first = session
            .send("first")
            .await
            .expect("first turn should succeed");
        drop(session);
        let mut resumed = harness
            .open_session(&database, "session-a")
            .await
            .expect("session should reopen");
        let second = resumed
            .send("second")
            .await
            .expect("second turn should succeed");

        // Assert
        assert_eq!(resumed.id(), "session-a");
        assert_eq!(first.output(), &json!({ "summary": "one" }));
        assert_eq!(second.output(), &json!({ "summary": "two" }));
    }

    #[tokio::test]
    async fn persistent_chat_does_not_store_failed_turns() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|_| Err(crate::ModelError::InvalidResponse));
        let harness = crate::Harness::new(model);
        let mut session = harness
            .create_session(&database, SessionConfig::new("session-a", schema()))
            .await
            .expect("session should be created");

        // Act
        let error = session.send("failed").await.expect_err("turn should fail");
        let message_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM session_message WHERE session_id = ?",
        )
        .bind("session-a")
        .fetch_one(&database.pool)
        .await
        .expect("message count should load");

        // Assert
        assert!(matches!(error, SessionError::Turn(_)));
        assert_eq!(message_count, 0);
    }

    #[tokio::test]
    async fn opening_session_validates_saved_model_identity() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let original = crate::Harness::new(metadata_model("provider-a", "model-a"));
        original
            .create_session(&database, SessionConfig::new("session-a", schema()))
            .await
            .expect("session should be created");
        let different = crate::Harness::new(metadata_model("provider-b", "model-b"));

        // Act
        let mismatch = different
            .open_session(&database, "session-a")
            .await
            .err()
            .expect("model mismatch should fail");
        let missing = different
            .open_session(&database, "missing")
            .await
            .err()
            .expect("missing session should fail");

        // Assert
        assert!(matches!(mismatch, SessionError::ModelMismatch { .. }));
        assert!(matches!(missing, SessionError::NotFound { .. }));
    }

    #[tokio::test]
    async fn opening_session_accepts_matching_model_identity() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let original = crate::Harness::new(metadata_model("provider", "model"));
        original
            .create_session(&database, SessionConfig::new("session-a", schema()))
            .await
            .expect("session should be created");
        let matching = crate::Harness::new(metadata_model("provider", "model"));

        // Act
        let session = matching
            .open_session(&database, "session-a")
            .await
            .expect("matching session should open");

        // Assert
        assert_eq!(session.id(), "session-a");
    }

    #[tokio::test]
    async fn opening_session_rejects_incomplete_saved_model_identity() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&SessionConfig::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&database.pool)
            .await
            .expect("constraints should be disabled for corruption fixture");
        sqlx::query("UPDATE session SET provider = 'provider' WHERE id = 'session-a'")
            .execute(&database.pool)
            .await
            .expect("model identity should be corrupted");
        let harness = crate::Harness::new(model());

        // Act
        let error = harness
            .open_session(&database, "session-a")
            .await
            .err()
            .expect("incomplete identity should fail");

        // Assert
        assert!(matches!(error, SessionError::InvalidData { .. }));
    }

    #[tokio::test]
    async fn persistent_chat_uses_saved_history_budget_when_reopened() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        let harness = crate::Harness::new(model())
            .max_history_bytes(NonZeroUsize::new(64).expect("history limit should be nonzero"));
        let session = harness
            .create_session(&database, SessionConfig::new("session-a", schema()))
            .await
            .expect("session should be created");
        drop(session);

        // Act
        let _reopened = harness
            .open_session(&database, "session-a")
            .await
            .expect("session should reopen");

        // Assert
        let loaded = database
            .load_session("session-a")
            .await
            .expect("session should load");
        assert_eq!(loaded.max_history_bytes, 64);
    }
}
