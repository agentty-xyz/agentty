use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use sqlx::SqlSafeStr as _;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::sqlite::SqlitePoolOptions;

use crate::model::{MockModel, ModelMessage, ModelMetadata};
use crate::schema_contract::OutputSchema;
use crate::session::{
    Database, DatabaseIdentity, DbResultExt as _, EncodedMessage, NewSession, ReservationObserver,
    SessionError, TimestampSource, TurnOwner, connect_options, next_turn_position,
    shared_abandoned_turn_registry, system_timestamp_source,
};
use crate::tool::{ReadArguments, ToolCall, WriteArguments};

pub(super) struct SessionTimestampsRow {
    pub(super) message_timestamp: i64,
    pub(super) session_created_at: i64,
    pub(super) session_updated_at: i64,
}

pub(super) struct TurnStatusRow {
    pub(super) message_count: i64,
    pub(super) status: String,
}

pub(super) fn schema() -> OutputSchema {
    OutputSchema::new(json!({
        "type": "object",
        "properties": { "summary": { "type": "string" } },
        "required": ["summary"],
        "additionalProperties": false
    }))
    .expect("schema should be valid")
}

pub(super) fn model() -> MockModel {
    let mut model = MockModel::new();
    model.expect_metadata().return_const(None);

    model
}

pub(super) fn metadata_model(provider: &'static str, model_name: &str) -> MockModel {
    let mut model = MockModel::new();
    let metadata = ModelMetadata::new(provider, model_name).expect("metadata should be valid");
    model.expect_metadata().return_const(Some(metadata));

    model
}

pub(super) fn read_call(id: &str) -> ToolCall {
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

pub(super) fn write_call(id: &str) -> ToolCall {
    let arguments = serde_json::from_value::<WriteArguments>(json!({
        "path": "src/lib.rs",
        "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
    }))
    .expect("write arguments should be valid");

    ToolCall::write(id.to_string(), arguments, None)
}

pub(super) fn turn(prompt: &str, answer: &str) -> Vec<ModelMessage> {
    vec![
        ModelMessage::User(prompt.to_string()),
        ModelMessage::Assistant(format!(r#"{{"summary":"{answer}"}}"#)),
    ]
}

pub(super) async fn complete_native_turn(database: &Database, provider_session_id: &str) {
    let mut acquired = database
        .begin_turn("session-a", "first")
        .await
        .expect("turn should begin");
    database
        .complete_turn(
            "session-a",
            acquired.turn_position,
            &turn("first", "first")[1..],
            Some(provider_session_id),
            &[],
            None,
        )
        .await
        .expect("turn should complete");
    acquired.guard.disarm();
}

pub(super) async fn active_turn_owner(
    database: &Database,
    session_id: &str,
    turn_position: i64,
) -> TurnOwner {
    let token = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT owner_token FROM session_turn WHERE session_id = ? AND turn_position = ?",
    )
    .bind(session_id)
    .bind(turn_position)
    .fetch_one(&database.pool)
    .await
    .expect("owner token should load");

    TurnOwner {
        database: database.identity.clone(),
        interruption_error_type: "interrupted",
        session_id: session_id.to_string(),
        token,
        turn_position,
    }
}

pub(super) type HistoricalMessage = (i64, i64, &'static str, &'static str, i64, i64);

pub(super) const MIGRATION_ONE_SNAPSHOT: &str = r"CREATE TABLE session (
    id TEXT PRIMARY KEY NOT NULL,
    provider TEXT,
    model TEXT,
    output_schema TEXT NOT NULL,
    system_prompt TEXT,
    max_history_bytes INTEGER NOT NULL CHECK (max_history_bytes > 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (
        (provider IS NULL AND model IS NULL)
        OR (provider IS NOT NULL AND model IS NOT NULL)
    )
);

CREATE TABLE session_message (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    turn_position INTEGER NOT NULL,
    message_position INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    retained_bytes INTEGER NOT NULL CHECK (retained_bytes >= 0),
    created_at INTEGER NOT NULL,
    UNIQUE (session_id, turn_position, message_position)
);

CREATE INDEX session_message_session_id_turn_position_idx
ON session_message (session_id, turn_position);
";

pub(super) async fn create_version_one_database(path: &Path) -> [HistoricalMessage; 4] {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options(path))
        .await
        .expect("version-one database should open");
    Migrator::with_migrations(vec![Migration::new(
        1,
        "create session".into(),
        MigrationType::Simple,
        MIGRATION_ONE_SNAPSHOT.into_sql_str(),
        false,
    )])
    .run(&pool)
    .await
    .expect("frozen migration one should apply");
    sqlx::query(
        r"
INSERT INTO session (
    id, provider, model, output_schema, system_prompt, max_history_bytes, created_at, updated_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
",
    )
    .bind("session-a")
    .bind("provider")
    .bind("model")
    .bind(schema().value().to_string())
    .bind("persistent instructions")
    .bind(100_000_i64)
    .bind(5_i64)
    .bind(30_i64)
    .execute(&pool)
    .await
    .expect("historical session should be inserted");
    let historical_messages = [
        (3_i64, 0_i64, "user", r#""first""#, 5_i64, 10_i64),
        (3, 1, "assistant", r#""{\"summary\":\"one\"}""#, 17, 11),
        (8, 0, "user", r#""second""#, 6, 20),
        (8, 1, "assistant", r#""{\"summary\":\"two\"}""#, 17, 21),
    ];
    for (turn_position, message_position, kind, payload, retained_bytes, created_at) in
        historical_messages
    {
        sqlx::query(
            r"
INSERT INTO session_message (
    session_id, turn_position, message_position, kind, payload, retained_bytes, created_at
)
VALUES (?, ?, ?, ?, ?, ?, ?)
",
        )
        .bind("session-a")
        .bind(turn_position)
        .bind(message_position)
        .bind(kind)
        .bind(payload)
        .bind(retained_bytes)
        .bind(created_at)
        .execute(&pool)
        .await
        .expect("historical message should be inserted");
    }
    pool.close().await;

    historical_messages
}

impl NewSession {
    /// Adds a system prompt that is restored with the session.
    #[must_use]
    pub(crate) fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());

        self
    }
}

impl Database {
    /// Opens an isolated in-memory SQLite database and runs migrations.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened or a migration
    /// fails.
    pub(crate) async fn open_in_memory() -> Result<Self, SessionError> {
        Self::open_in_memory_with_timestamp_source(system_timestamp_source()).await
    }

    /// Opens an in-memory database with an injected timestamp source.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened or a migration
    /// fails.
    pub(crate) async fn open_in_memory_with_timestamp_source(
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
            abandoned_turns: shared_abandoned_turn_registry(),
            identity: DatabaseIdentity::temporary(),
            pool,
            reservation_observer: Arc::new(()),
            timestamp_source,
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
        sqlx::query(
            r"
INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at
)
VALUES (?, ?, 'completed', NULL, NULL, ?, ?)
",
        )
        .bind(session_id)
        .bind(turn_position)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .session_context("append persistent session turn")?;

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
}

#[derive(Default)]
pub(super) struct ReservationCommitControl {
    pub(super) commit_seen: std::sync::atomic::AtomicBool,
    pub(super) pause_once: std::sync::atomic::AtomicBool,
}

impl ReservationCommitControl {
    pub(super) fn paused() -> Self {
        Self {
            commit_seen: std::sync::atomic::AtomicBool::new(false),
            pause_once: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn pause_after_commit(&self) -> bool {
        if !self
            .pause_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return false;
        }
        self.commit_seen
            .store(true, std::sync::atomic::Ordering::SeqCst);

        true
    }
}

#[async_trait]
impl ReservationObserver for ReservationCommitControl {
    async fn committed(&self) {
        if self.pause_after_commit() {
            std::future::pending::<()>().await;
        }
    }
}
