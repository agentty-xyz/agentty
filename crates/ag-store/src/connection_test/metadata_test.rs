//! Session metadata persistence tests for conversation IDs, upstream refs,
//! merged commits, and turn metadata.

use ag_contracts::TurnUsage;
use ag_contracts::test_support::token_usage;
use ag_session::test_support as model_fixture;

use super::support::insert_session_fixture;
use crate::SessionTurnMetadata;
use crate::connection::Database;
use crate::error::DbError;

/// Verifies `load_sessions_metadata()` counts sessions in every project while
/// taking the latest `updated_at` from the requested project only.
#[tokio::test]
async fn test_load_sessions_metadata_returns_count_and_latest_project_timestamp() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    let other_project_id = database
        .projects()
        .upsert_project("/tmp/other-project", Some("main".to_string()))
        .await
        .expect("failed to insert other project");
    let empty_project_id = database
        .projects()
        .upsert_project("/tmp/empty-project", Some("main".to_string()))
        .await
        .expect("failed to insert empty project");

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    insert_session_fixture(&database, "session-b", "main", "Done", project_id).await;
    insert_session_fixture(&database, "session-c", "main", "Review", other_project_id).await;
    for (session_id, updated_at) in [("session-a", 200), ("session-b", 300), ("session-c", 900)] {
        database
            .sessions()
            .update_session_updated_at(session_id, updated_at)
            .await
            .expect("failed to update session updated_at");
    }

    // Act
    let session_metadata = database
        .sessions()
        .load_sessions_metadata(project_id)
        .await
        .expect("failed to load session metadata");
    let empty_project_metadata = database
        .sessions()
        .load_sessions_metadata(empty_project_id)
        .await
        .expect("failed to load empty project metadata");

    // Assert
    assert_eq!(session_metadata, (3, 300));
    assert_eq!(empty_project_metadata, (3, 0));
}

#[tokio::test]
async fn test_session_provider_conversation_id_round_trip_and_clear() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .sessions()
        .update_session_provider_conversation_id("session-a", Some("thread-123".to_string()))
        .await
        .expect("failed to set provider conversation id");
    let stored_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load provider conversation id");
    database
        .sessions()
        .update_session_provider_conversation_id("session-a", None)
        .await
        .expect("failed to clear provider conversation id");
    let cleared_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load cleared provider conversation id");

    // Assert
    assert_eq!(stored_id, Some("thread-123".to_string()));
    assert_eq!(cleared_id, None);
}

#[tokio::test]
async fn test_session_instruction_conversation_id_round_trip_and_clear() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");
    let instruction_conversation_id = Some("thread-123");

    // Act
    database
        .sessions()
        .update_session_instruction_conversation_id(
            "session-a",
            instruction_conversation_id.map(str::to_string),
        )
        .await
        .expect("failed to set instruction conversation id");
    let stored_conversation_id = database
        .sessions()
        .get_session_instruction_conversation_id("session-a")
        .await
        .expect("failed to load instruction conversation id");
    database
        .sessions()
        .update_session_instruction_conversation_id("session-a", None)
        .await
        .expect("failed to clear instruction conversation id");
    let cleared_conversation_id = database
        .sessions()
        .get_session_instruction_conversation_id("session-a")
        .await
        .expect("failed to load cleared instruction conversation id");

    // Assert
    assert_eq!(stored_conversation_id, Some("thread-123".to_string()));
    assert_eq!(cleared_conversation_id, None);
}

#[tokio::test]
async fn test_session_published_upstream_ref_round_trip_and_clear() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .sessions()
        .update_session_published_upstream_ref("session-a", Some("origin/wt/session-a".to_string()))
        .await
        .expect("failed to persist session published upstream ref");
    let persisted_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .find(|row| row.id == "session-a")
        .expect("missing persisted session row");
    database
        .sessions()
        .update_session_published_upstream_ref("session-a", None)
        .await
        .expect("failed to clear session published upstream ref");
    let cleared_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions after clearing")
        .into_iter()
        .find(|row| row.id == "session-a")
        .expect("missing cleared session row");

    // Assert
    assert_eq!(
        persisted_row.published_upstream_ref.as_deref(),
        Some("origin/wt/session-a")
    );
    assert_eq!(cleared_row.published_upstream_ref, None);
}

#[tokio::test]
async fn test_load_session_published_upstream_ref_returns_stored_value() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-load", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    database
        .sessions()
        .update_session_published_upstream_ref(
            "session-load",
            Some("origin/wt/session-load".to_string()),
        )
        .await
        .expect("failed to set published upstream ref");

    // Act
    let loaded_ref = database
        .sessions()
        .load_session_published_upstream_ref("session-load")
        .await
        .expect("failed to load published upstream ref");

    // Assert
    assert_eq!(loaded_ref.as_deref(), Some("origin/wt/session-load"));
}

#[tokio::test]
async fn test_load_session_published_upstream_ref_returns_none_when_unset() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-unset", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");

    // Act
    let loaded_ref = database
        .sessions()
        .load_session_published_upstream_ref("session-unset")
        .await
        .expect("failed to load published upstream ref");

    // Assert
    assert_eq!(loaded_ref, None);
}

#[tokio::test]
async fn test_load_session_published_upstream_ref_returns_none_for_missing_session() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");

    // Act
    let loaded_ref = database
        .sessions()
        .load_session_published_upstream_ref("nonexistent")
        .await
        .expect("failed to load published upstream ref");

    // Assert
    assert_eq!(loaded_ref, None);
}

#[tokio::test]
async fn test_session_merged_commit_hash_round_trip_and_clear() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .sessions()
        .update_session_merged_commit_hash("session-a", Some("abc1234".to_string()))
        .await
        .expect("failed to store merged commit hash");
    let stored_hash = database
        .sessions()
        .load_session_merged_commit_hash("session-a")
        .await
        .expect("failed to load stored merged commit hash");
    database
        .sessions()
        .update_session_merged_commit_hash("session-a", None)
        .await
        .expect("failed to clear merged commit hash");
    let cleared_hash = database
        .sessions()
        .load_session_merged_commit_hash("session-a")
        .await
        .expect("failed to load cleared merged commit hash");

    // Assert
    assert_eq!(stored_hash.as_deref(), Some("abc1234"));
    assert_eq!(cleared_hash, None);
}

#[tokio::test]
/// Verifies transactional turn-metadata persistence rolls back partial
/// writes when any statement in the transaction fails.
async fn test_persist_session_turn_metadata_rolls_back_on_failure() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    sqlx::query!("DROP TABLE session_usage")
        .execute(database.pool())
        .await
        .expect("failed to drop session-usage table");

    // Act
    let result = database
        .sessions()
        .persist_session_turn_metadata(
            "session-a",
            &SessionTurnMetadata {
                harness_turn_report: None,
                instruction_conversation_id: Some("instruction-thread".to_string()),
                model: model_fixture::CODEX_MODEL.as_str().to_string(),
                provider_conversation_id: Some("thread-123".to_string()),
                questions_json: r#"[{"text":"Need tests?"}]"#.to_string(),
                review_comment_resolutions: Vec::new(),
                usage: TurnUsage {
                    checkpoint: Some("checkpoint".to_string()),
                    ..token_usage(model_fixture::CODEX_MODEL.as_str(), 3, 5)
                },
            },
        )
        .await;
    let session = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to reload sessions")
        .into_iter()
        .find(|session| session.id == "session-a")
        .expect("expected seeded session");
    let provider_conversation_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load provider conversation id");
    let usage_checkpoint = database
        .sessions()
        .get_session_usage_checkpoint("session-a")
        .await
        .expect("failed to load usage checkpoint");
    let token_totals: (i64, i64) =
        sqlx::query_as("SELECT input_tokens, output_tokens FROM session WHERE id = 'session-a'")
            .fetch_one(database.pool())
            .await
            .expect("failed to load session token totals");

    // Assert
    assert!(matches!(result, Err(DbError::Query(_))));
    assert_eq!(session.questions.as_deref(), None);
    assert_eq!(token_totals, (0, 0));
    assert_eq!(provider_conversation_id.as_deref(), None);
    assert_eq!(usage_checkpoint, None);
}

/// Verifies each turn's reported usage checkpoint replaces the session's, a
/// turn without one clears it, and a missing session reads none.
#[tokio::test]
async fn test_persist_session_turn_metadata_replaces_usage_checkpoint() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let initial = database
        .sessions()
        .get_session_usage_checkpoint("session-a")
        .await
        .expect("failed to load initial checkpoint");
    let mut checkpoints = Vec::new();

    // Act
    for checkpoint in [Some("first"), Some("second"), None] {
        database
            .sessions()
            .persist_session_turn_metadata(
                "session-a",
                &SessionTurnMetadata {
                    harness_turn_report: None,
                    instruction_conversation_id: None,
                    model: "gpt-5.6-sol".to_string(),
                    provider_conversation_id: None,
                    questions_json: String::new(),
                    review_comment_resolutions: Vec::new(),
                    usage: TurnUsage {
                        checkpoint: checkpoint.map(str::to_string),
                        ..TurnUsage::default()
                    },
                },
            )
            .await
            .expect("failed to persist turn metadata");
        checkpoints.push(
            database
                .sessions()
                .get_session_usage_checkpoint("session-a")
                .await
                .expect("failed to load checkpoint"),
        );
    }
    let missing = database
        .sessions()
        .get_session_usage_checkpoint("missing")
        .await
        .expect("failed to load missing session checkpoint");

    // Assert
    assert_eq!(initial, None);
    assert_eq!(
        checkpoints,
        [Some("first"), Some("second"), None].map(|value| value.map(str::to_string))
    );
    assert_eq!(missing, None);
}

/// Verifies a harness turn report is stored with its turn and outlives the
/// session for later analysis.
#[tokio::test]
async fn test_persist_session_turn_metadata_keeps_harness_turn_report_after_session_deletion() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "muse-spark-1.3", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let report = r#"{"model_requests":[]}"#;

    // Act
    database
        .sessions()
        .persist_session_turn_metadata(
            "session-a",
            &SessionTurnMetadata {
                harness_turn_report: Some(report.to_string()),
                instruction_conversation_id: None,
                model: "muse-spark-1.3".to_string(),
                provider_conversation_id: None,
                questions_json: String::new(),
                review_comment_resolutions: Vec::new(),
                usage: token_usage("muse-spark-1.3", 30, 7),
            },
        )
        .await
        .expect("failed to persist turn metadata");
    let stored = load_harness_turn_reports(&database).await;
    database
        .sessions()
        .delete_session("session-a")
        .await
        .expect("failed to delete session");
    let retained = load_harness_turn_reports(&database).await;

    // Assert
    assert_eq!(
        stored,
        [(
            Some("session-a".to_string()),
            "muse-spark-1.3".to_string(),
            report.to_string()
        )]
    );
    assert_eq!(
        retained,
        [(None, "muse-spark-1.3".to_string(), report.to_string())]
    );
}

/// Loads every stored harness turn report as `(session_id, model, report)`.
async fn load_harness_turn_reports(database: &Database) -> Vec<(Option<String>, String, String)> {
    sqlx::query_as("SELECT session_id, model, report FROM harness_turn_report ORDER BY id")
        .fetch_all(database.pool())
        .await
        .expect("failed to load harness turn reports")
}
