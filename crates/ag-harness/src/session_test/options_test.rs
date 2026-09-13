use std::num::NonZeroUsize;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;

use super::support::{create_version_one_database, schema, turn_options};
use crate::session::{Database, NewSession, SessionError, StoredTurnOptions};
use crate::{ComparisonBase, OutputSchema, Tool, ToolPolicy, TurnError, TurnLimits, TurnOptions};

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
        ("version", json!(4)),
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
    assert!(decoded.continuation_compatible(&options));
    assert_eq!(decoded.max_tool_calls, options.limits().max_tool_calls());
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
    assert!(
        StoredTurnOptions::decode(&running.1)
            .expect("running options")
            .continuation_compatible(&options)
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
    sqlx::query("UPDATE session_turn SET turn_options = json_set(turn_options, '$.version', 4)")
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

#[test]
fn comparison_snapshots_preserve_identity_and_fingerprint_all_effective_options() {
    // Arrange
    let plain = turn_options();
    let selected = plain
        .clone()
        .with_comparison_base(ComparisonBase::fixture("deleted-repository"));
    let other_scope = plain
        .clone()
        .with_comparison_base(ComparisonBase::fixture("other-repository"));
    let budget = TurnOptions::new(
        plain.schema().clone(),
        plain.tool_policy(),
        TurnLimits::new(NonZeroUsize::new(17).expect("budget")),
    );

    // Act
    let snapshots: Vec<_> = [&plain, &selected, &other_scope, &budget]
        .into_iter()
        .map(|options| {
            StoredTurnOptions::decode(&StoredTurnOptions::encode(options)).expect("stored metadata")
        })
        .collect();
    let fingerprints: Vec<_> = snapshots
        .iter()
        .map(StoredTurnOptions::fingerprint)
        .collect();

    // Assert
    assert!(snapshots[1].continuation_compatible(&selected));
    assert!(!snapshots[1].continuation_compatible(&plain));
    assert!(!snapshots[1].continuation_compatible(&other_scope));
    assert!(snapshots[0].continuation_compatible(&budget));
    for (index, fingerprint) in fingerprints.iter().enumerate() {
        assert_eq!(fingerprint.len(), 64);
        assert!(!fingerprints[..index].contains(fingerprint));
    }
}

#[test]
fn version_three_fingerprints_sort_nested_object_keys_and_preserve_array_order() {
    // Arrange
    let schemas = [
        r#"{"type":"array","prefixItems":[{"type":"string","minLength":1},{"type":"integer","minimum":0}]}"#,
        r#"{"prefixItems":[{"minLength":1,"type":"string"},{"minimum":0,"type":"integer"}],"type":"array"}"#,
        r#"{"type":"array","prefixItems":[{"type":"integer","minimum":0},{"type":"string","minLength":1}]}"#,
    ];
    let options: Vec<_> = schemas
        .iter()
        .map(|schema| {
            TurnOptions::new(
                OutputSchema::new(serde_json::from_str(schema).expect("schema JSON"))
                    .expect("schema"),
                ToolPolicy::default(),
                TurnLimits::new(NonZeroUsize::new(8).expect("budget")),
            )
        })
        .collect();

    // Act
    let snapshots: Vec<_> = options
        .iter()
        .map(|options| {
            StoredTurnOptions::decode(&StoredTurnOptions::encode(options)).expect("snapshot")
        })
        .collect();
    let mut reordered: Value =
        serde_json::from_str(&StoredTurnOptions::encode(&options[0])).expect("snapshot JSON");
    reordered["output_schema"] = options[1].schema().value().clone();
    let restored = StoredTurnOptions::decode(&reordered.to_string()).expect("reordered snapshot");

    // Assert
    assert_eq!(snapshots[0].version, 3);
    assert_eq!(
        snapshots[0].fingerprint(),
        "ff322e5ad3b6da9df9bcead8ddd1d5f58157a4351f0a1c52756b67f29e1c71d4"
    );
    assert_eq!(snapshots[0].fingerprint(), snapshots[1].fingerprint());
    assert_ne!(snapshots[0].fingerprint(), snapshots[2].fingerprint());
    assert!(restored.continuation_compatible(&options[0]));
    assert!(!restored.continuation_compatible(&options[2]));
}

#[tokio::test]
async fn version_two_snapshots_keep_their_fingerprint_rules_and_native_continuation() {
    // Arrange
    let options = turn_options();
    let mut legacy = json!({
        "comparison_base": null,
        "max_tool_calls": options.limits().max_tool_calls(),
        "output_schema": options.schema().value(),
        "tool_policy": options.tool_policy(),
        "version": 2,
    });
    let fingerprint = format!("{:x}", Sha256::digest(legacy.to_string()));
    legacy["fingerprint"] = json!(fingerprint);
    let directory = tempdir().expect("temporary directory");
    let database_path = directory.path().join("history.db");
    let database = Database::open(&database_path).await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 4096)
        .await
        .expect("session");
    let mut first = database
        .begin_turn("session", "first", &options)
        .await
        .expect("first turn");
    database
        .complete_turn("session", first.turn_position, &[], Some("native"))
        .await
        .expect("completed turn");
    first.guard.disarm();
    sqlx::query("UPDATE session_turn SET turn_options = ? WHERE turn_position = 0")
        .bind(legacy.to_string())
        .execute(database.pool())
        .await
        .expect("legacy snapshot");

    // Act
    let stored = StoredTurnOptions::decode(&legacy.to_string()).expect("legacy metadata");
    let reopened = Database::open(&database_path)
        .await
        .expect("reopened database");
    reopened
        .load_session("session")
        .await
        .expect("legacy history");
    let acquired = reopened
        .begin_turn("session", "next", &options)
        .await
        .expect("next turn");
    let snapshot: String =
        sqlx::query_scalar("SELECT turn_options FROM session_turn WHERE turn_position = 1")
            .fetch_one(reopened.pool())
            .await
            .expect("new snapshot");

    // Assert
    assert_eq!(stored.fingerprint(), fingerprint);
    assert!(stored.continuation_compatible(&options));
    assert_eq!(acquired.provider_session_id.as_deref(), Some("native"));
    assert_eq!(
        StoredTurnOptions::decode(&snapshot)
            .expect("new metadata")
            .version,
        3
    );
}

#[test]
fn version_one_options_remain_readable_but_never_imply_a_known_base() {
    // Arrange
    let options = turn_options();
    let legacy = json!({"version":1, "output_schema":options.schema().value(), "tool_policy":options.tool_policy(), "max_tool_calls":options.limits().max_tool_calls()});
    let mut conflicting = legacy.clone();
    conflicting["comparison_base"] = json!(ComparisonBase::fixture("repo").identity());

    // Act
    let stored = StoredTurnOptions::decode(&legacy.to_string()).expect("legacy options");
    let invalid = StoredTurnOptions::decode(&conflicting.to_string());

    // Assert
    assert!(stored.comparison_base.is_none());
    assert!(!stored.continuation_compatible(&options));
    assert!(invalid.is_err());
}

#[test]
fn comparison_metadata_and_fingerprint_corruption_are_rejected() {
    // Arrange
    let options = turn_options().with_comparison_base(ComparisonBase::fixture("repo"));
    let mut snapshot: Value =
        serde_json::from_str(&StoredTurnOptions::encode(&options)).expect("snapshot");
    snapshot["comparison_base"]["oid"] = json!("HEAD");
    let stored: StoredTurnOptions =
        serde_json::from_value(snapshot.clone()).expect("typed metadata");
    snapshot["fingerprint"] = json!(stored.fingerprint());
    let mut mismatched: Value =
        serde_json::from_str(&StoredTurnOptions::encode(&options)).expect("snapshot");
    mismatched["fingerprint"] = json!("incorrect");

    // Act / Assert
    assert!(StoredTurnOptions::decode(&snapshot.to_string()).is_err());
    assert!(StoredTurnOptions::decode(&mismatched.to_string()).is_err());
}
