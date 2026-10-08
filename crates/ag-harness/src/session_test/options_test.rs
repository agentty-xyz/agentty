use std::sync::Arc;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;

use super::support::{create_version_one_database, schema, turn_options};
use crate::input::TurnInput;
use crate::session::{Database, NewSession, SessionError};
use crate::store::{AcquiredTurn, SessionStore as _};
use crate::turn_options_snapshot::StoredTurnOptions;
use crate::{OutputSchema, TurnError};

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
    let mut first = AcquiredTurn::begin(
        Arc::new(database.clone()),
        "session",
        &TurnInput::from("failed"),
        &options,
        0,
    )
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
            first.guard.owner().turn_position,
            &TurnError::ToolDenied {
                name: "read".to_string(),
            },
        )
        .await
        .expect("failed turn");
    first.guard.disarm();
    let second = AcquiredTurn::begin(
        Arc::new(database.clone()),
        "session",
        &TurnInput::from("interrupted"),
        &options,
        0,
    )
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
    assert!(StoredTurnOptions::decode(&running.1).is_ok());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "failed");
    assert_eq!(rows[1].0, "interrupted");
    assert!(rows.iter().all(|(_, snapshot)| snapshot == &running.1));
}

#[tokio::test]
async fn legacy_history_keeps_its_schema_after_dropping_native_continuation() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let database_path = directory.path().join("legacy.db");
    create_version_one_database(&database_path).await;
    let database = Database::open(&database_path)
        .await
        .expect("migrated database");

    // Act
    let loaded = database
        .load_session("session-a")
        .await
        .expect("legacy session");
    let acquired = AcquiredTurn::begin(
        Arc::new(database.clone()),
        "session-a",
        &TurnInput::from("new"),
        &turn_options(),
        0,
    )
    .await
    .expect("new turn");
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('session')")
        .fetch_all(database.pool())
        .await
        .expect("session columns");

    // Assert
    assert_eq!(loaded.schema, schema());
    assert_eq!(acquired.turns.len(), 2);
    assert!(!columns.iter().any(|column| column == "provider_session_id"));
}

#[tokio::test]
async fn corrupt_snapshots_are_rejected_on_reopen_and_before_reservation() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 4096)
        .await
        .expect("session");
    let mut first = AcquiredTurn::begin(
        Arc::new(database.clone()),
        "session",
        &TurnInput::from("first"),
        &turn_options(),
        0,
    )
    .await
    .expect("reservation");
    database
        .complete_turn("session", first.guard.owner().turn_position, &[])
        .await
        .expect("completion");
    first.guard.disarm();
    let encoded = StoredTurnOptions::encode(&turn_options());
    let mut unsupported: Value = serde_json::from_str(&encoded).expect("snapshot");
    unsupported["version"] = json!(6);
    let invalid_schema = json!({"type": "invalid"});
    let schema_error = OutputSchema::new(invalid_schema.clone()).expect_err("invalid schema");
    let mut invalid: Value = serde_json::from_str(&encoded).expect("snapshot");
    invalid["output_schema"] = invalid_schema;
    let cases = [
        (
            unsupported.to_string(),
            "invalid persistent session data: unsupported turn options version 6".to_string(),
            false,
        ),
        (
            "null".to_string(),
            "invalid persistent session data: invalid persistent message JSON: invalid type: \
             null, expected struct StoredTurnOptions at line 1 column 4"
                .to_string(),
            false,
        ),
        (invalid.to_string(), schema_error.to_string(), true),
    ];

    // Act / Assert
    for (snapshot, expected, schema_error) in cases {
        sqlx::query("UPDATE session_turn SET turn_options = ?")
            .bind(snapshot)
            .execute(database.pool())
            .await
            .expect("corrupt snapshot");
        let loaded = database
            .load_session("session")
            .await
            .err()
            .expect("reopen error");
        let reserved = AcquiredTurn::begin(
            Arc::new(database.clone()),
            "session",
            &TurnInput::from("second"),
            &turn_options(),
            0,
        )
        .await
        .err()
        .expect("reservation error");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_turn")
            .fetch_one(database.pool())
            .await
            .expect("turn count");

        for error in [loaded, reserved] {
            assert_eq!(error.to_string(), expected);
            if schema_error {
                assert!(matches!(error, SessionError::Schema(_)));
            } else {
                assert!(matches!(error, SessionError::InvalidData { .. }));
            }
        }
        assert_eq!(count, 1);
    }
}

#[tokio::test]
async fn version_two_snapshots_keep_their_fingerprint_rules() {
    // Arrange
    let options = turn_options();
    let mut legacy = json!({
        "comparison_base": null,
        "max_tool_calls": 8,
        "output_schema": options.schema().value(),
        "tool_policy": options.tool_policy(),
        "version": 2,
    });
    let fingerprint = hex::encode(Sha256::digest(legacy.to_string()));
    legacy["fingerprint"] = json!(fingerprint);
    let directory = tempdir().expect("temporary directory");
    let database_path = directory.path().join("history.db");
    let database = Database::open(&database_path).await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 4096)
        .await
        .expect("session");
    let mut first = AcquiredTurn::begin(
        Arc::new(database.clone()),
        "session",
        &TurnInput::from("first"),
        &options,
        0,
    )
    .await
    .expect("first turn");
    database
        .complete_turn("session", first.guard.owner().turn_position, &[])
        .await
        .expect("completed turn");
    first.guard.disarm();
    sqlx::query("UPDATE session_turn SET turn_options = ? WHERE turn_position = 0")
        .bind(legacy.to_string())
        .execute(database.pool())
        .await
        .expect("legacy snapshot");

    // Act
    let stored = StoredTurnOptions::decode(&legacy.to_string());
    let reopened = Database::open(&database_path)
        .await
        .expect("reopened database");
    reopened
        .load_session("session")
        .await
        .expect("legacy history");
    AcquiredTurn::begin(
        Arc::new(reopened.clone()),
        "session",
        &TurnInput::from("next"),
        &options,
        0,
    )
    .await
    .expect("next turn");
    let snapshot: String =
        sqlx::query_scalar("SELECT turn_options FROM session_turn WHERE turn_position = 1")
            .fetch_one(reopened.pool())
            .await
            .expect("new snapshot");

    // Assert
    assert!(stored.is_ok());
    assert_eq!(
        serde_json::from_str::<Value>(&snapshot).expect("new metadata")["version"],
        5
    );
}
