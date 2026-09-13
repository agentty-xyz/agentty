use std::num::NonZeroUsize;

use serde_json::{Value, json};
use tempfile::tempdir;

use super::support::{create_version_one_database, schema, turn_options};
use crate::session::{Database, NewSession, SessionError, StoredTurnOptions};
use crate::{Tool, ToolPolicy, TurnError, TurnLimits, TurnOptions};

#[test]
fn snapshots_round_trip_and_reject_unknown_or_invalid_configuration() {
    // Arrange
    let options = TurnOptions::new(
        schema(),
        ToolPolicy::default().allow(Tool::Write),
        TurnLimits::new(NonZeroUsize::new(3).expect("nonzero budget")),
    );
    let encoded = StoredTurnOptions::encode(&options);
    let snapshot: Value = serde_json::from_str(&encoded).expect("snapshot JSON");
    let mut invalid = Vec::new();
    for (key, value) in [
        ("version", json!(2)),
        ("max_tool_calls", json!(0)),
        ("output_schema", json!({"type":"invalid"})),
        ("tool_policy", json!({"read":true})),
        ("unknown", json!(true)),
    ] {
        let mut value_snapshot = snapshot.clone();
        value_snapshot[key] = value;
        invalid.push(value_snapshot.to_string());
    }
    invalid.push("invalid JSON".to_string());

    // Act
    let decoded = StoredTurnOptions::decode(&encoded).expect("decode snapshot");
    let errors: Vec<_> = invalid
        .iter()
        .map(|snapshot| StoredTurnOptions::decode(snapshot))
        .collect();

    // Assert
    assert_eq!(decoded, options);
    assert!(errors.iter().all(Result::is_err));
}

#[tokio::test]
async fn snapshots_are_committed_before_execution_and_survive_failure_and_interruption() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 4096)
        .await
        .expect("session");
    let options = turn_options();

    // Act
    let mut first = database
        .begin_turn("session", "failed", &options)
        .await
        .expect("reservation");
    let running: (String, String) =
        sqlx::query_as("SELECT status, turn_options FROM session_turn WHERE turn_position = 0")
            .fetch_one(database.pool())
            .await
            .expect("running snapshot");
    database
        .fail_turn(
            "session",
            first.turn_position,
            &TurnError::ToolCallLimit { limit: 8 },
        )
        .await
        .expect("failed turn");
    first.guard.disarm();
    let second = database
        .begin_turn("session", "interrupted", &options)
        .await
        .expect("second reservation");
    drop(second);
    database
        .load_session("session")
        .await
        .expect("recover session");
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT status, turn_options FROM session_turn ORDER BY turn_position")
            .fetch_all(database.pool())
            .await
            .expect("retained snapshots");

    // Assert
    assert_eq!(running.0, "running");
    assert_eq!(
        StoredTurnOptions::decode(&running.1).expect("running options"),
        options
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "failed");
    assert_eq!(rows[1].0, "interrupted");
    assert!(rows.iter().all(|(_, snapshot)| snapshot == &running.1));
}

#[tokio::test]
async fn legacy_history_keeps_its_schema_but_replays_unknown_native_configuration() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let database_path = directory.path().join("legacy.db");
    create_version_one_database(&database_path).await;
    let database = Database::open(&database_path)
        .await
        .expect("migrated database");
    sqlx::query("UPDATE session SET provider_session_id = 'legacy-native' WHERE id = 'session-a'")
        .execute(database.pool())
        .await
        .expect("legacy continuation");

    // Act
    let loaded = database
        .load_session("session-a")
        .await
        .expect("legacy session");
    let acquired = database
        .begin_turn("session-a", "new", &turn_options())
        .await
        .expect("new turn");
    let native: Option<String> =
        sqlx::query_scalar("SELECT provider_session_id FROM session WHERE id = 'session-a'")
            .fetch_one(database.pool())
            .await
            .expect("canonical continuation");

    // Assert
    assert_eq!(loaded.schema, schema());
    assert_eq!(acquired.turns.len(), 2);
    assert!(acquired.provider_session_id.is_none());
    assert!(native.is_none());
}

#[tokio::test]
async fn corrupt_snapshots_are_rejected_on_reopen_and_before_reservation() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 4096)
        .await
        .expect("session");
    let mut first = database
        .begin_turn("session", "first", &turn_options())
        .await
        .expect("reservation");
    database
        .complete_turn("session", first.turn_position, &[], Some("native"))
        .await
        .expect("completion");
    first.guard.disarm();
    sqlx::query("UPDATE session_turn SET turn_options = json_set(turn_options, '$.version', 2)")
        .execute(database.pool())
        .await
        .expect("corrupt version");

    // Act
    let loaded = database.load_session("session").await;
    let reserved = database
        .begin_turn("session", "second", &turn_options())
        .await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_turn")
        .fetch_one(database.pool())
        .await
        .expect("turn count");

    // Assert
    assert!(matches!(loaded, Err(SessionError::InvalidData { .. })));
    assert!(matches!(reserved, Err(SessionError::InvalidData { .. })));
    assert_eq!(count, 1);
}
