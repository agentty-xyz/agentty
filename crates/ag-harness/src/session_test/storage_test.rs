use std::path::Path;
use std::sync::Arc;

use tempfile::tempdir;

use super::support::{
    SessionTimestampsRow, create_version_one_database, read_call, schema, turn, write_call,
};
use crate::model::{ModelMessage, ModelMetadata};
use crate::session::{Database, EncodedMessage, NewSession, SessionError, TimestampSource};

#[test]
fn session_config_exposes_values_and_system_prompt() {
    // Arrange
    let schema = schema();

    // Act
    let config =
        NewSession::new("session-a", schema.clone()).with_system_prompt("persistent instructions");

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
async fn migration_two_backfills_historical_turn_lifecycle_without_data_loss() {
    // Arrange
    let temp_dir = tempdir().expect("temporary directory should be created");
    let database_path = temp_dir.path().join("harness.db");
    let historical_messages = create_version_one_database(&database_path).await;

    // Act
    let database = Database::open(&database_path)
        .await
        .expect("database should upgrade to migration two");
    let loaded = database
        .load_session("session-a")
        .await
        .expect("upgraded session should load");
    let session_row = sqlx::query_as::<_, (i64, i64, Option<String>)>(
        "SELECT created_at, updated_at, provider_session_id FROM session WHERE id = ?",
    )
    .bind("session-a")
    .fetch_one(database.pool())
    .await
    .expect("upgraded session row should load");
    let turns = sqlx::query_as::<_, (i64, String, Option<String>, Option<i64>, i64, i64)>(
        r"
SELECT turn_position, status, error_type, lease_expires_at, created_at, updated_at
FROM session_turn
WHERE session_id = ?
ORDER BY turn_position
",
    )
    .bind("session-a")
    .fetch_all(database.pool())
    .await
    .expect("backfilled turns should load");
    let messages = sqlx::query_as::<_, (i64, i64, String, String, i64, i64)>(
        r"
SELECT turn_position, message_position, kind, payload, retained_bytes, created_at
FROM session_message
WHERE session_id = ?
ORDER BY turn_position, message_position
",
    )
    .bind("session-a")
    .fetch_all(database.pool())
    .await
    .expect("historical messages should load");

    // Assert
    assert_eq!(session_row, (5, 30, None));
    assert_eq!(
        turns,
        vec![
            (3, "completed".to_string(), None, None, 10, 11),
            (8, "completed".to_string(), None, None, 20, 21),
        ]
    );
    assert_eq!(
        messages,
        historical_messages
            .into_iter()
            .map(
                |(turn_position, message_position, kind, payload, retained_bytes, created_at)| {
                    (
                        turn_position,
                        message_position,
                        kind.to_string(),
                        payload.to_string(),
                        retained_bytes,
                        created_at,
                    )
                },
            )
            .collect::<Vec<_>>()
    );
    assert_eq!(
        loaded.turns,
        vec![turn("first", "one"), turn("second", "two")]
    );
}

#[tokio::test]
async fn database_round_trips_every_persistent_message_kind() {
    // Arrange
    let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 456))
        .await
        .expect("database should open");
    let config =
        NewSession::new("session-a", schema()).with_system_prompt("persistent instructions");
    let metadata = ModelMetadata::new("provider", "model").expect("metadata should be valid");
    database
        .create_session(&config, Some(metadata), 100_000)
        .await
        .expect("session should be created");
    let messages = vec![
        ModelMessage::User("inspect and edit".to_string()),
        ModelMessage::AssistantReasoning {
            content: r#"{"summary":"thinking"}"#.to_string(),
            reasoning_content: "I should inspect before editing.".to_string(),
        },
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
    let timestamps = sqlx::query_as!(
        SessionTimestampsRow,
        r#"
SELECT session.created_at AS "session_created_at!: i64",
       session.updated_at AS "session_updated_at!: i64",
       session_message.created_at AS "message_timestamp!: i64"
FROM session
INNER JOIN session_message ON session_message.session_id = session.id
WHERE session.id = ?
LIMIT 1
"#,
        "session-a"
    )
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
    assert_eq!(timestamps.message_timestamp, 456);
    assert_eq!(timestamps.session_created_at, 456);
    assert_eq!(timestamps.session_updated_at, 456);
}

#[tokio::test]
async fn database_reports_creation_and_loading_errors() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    let config = NewSession::new("session-a", schema());
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
        .create_session(&NewSession::new(" ", schema()), None, 100)
        .await
        .expect_err("empty identifier should fail");
    let oversized_limit = database
        .create_session(&NewSession::new("session-b", schema()), None, usize::MAX)
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
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
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
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
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
    sqlx::query("UPDATE session_message SET retained_bytes = -1 WHERE session_id = 'session-a'")
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
