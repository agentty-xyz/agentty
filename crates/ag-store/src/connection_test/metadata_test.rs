use ag_agent::{AgentModel, SessionDiffState, SessionStats};

use super::support::insert_session_fixture;
use crate::SessionTurnMetadata;
use crate::connection::Database;
use crate::error::DbError;

/// Verifies `load_sessions_metadata()` returns session count and max
/// `updated_at`.
#[tokio::test]
async fn test_load_sessions_metadata_returns_count_and_latest_timestamp() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    insert_session_fixture(&database, "session-b", "main", "Done", project_id).await;
    database
        .sessions()
        .update_session_updated_at("session-a", 200)
        .await
        .expect("failed to update session-a updated_at");
    database
        .sessions()
        .update_session_updated_at("session-b", 300)
        .await
        .expect("failed to update session-b updated_at");

    // Act
    let session_metadata = database
        .sessions()
        .load_sessions_metadata()
        .await
        .expect("failed to load session metadata");

    // Assert
    assert_eq!(session_metadata, (2, 300));
}

#[tokio::test]
async fn test_session_provider_conversation_id_round_trip_and_clear() {
    // Arrange
    let database = Database::open_in_memory()
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
    let database = Database::open_in_memory()
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
    let database = Database::open_in_memory()
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
    let database = Database::open_in_memory()
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
    let database = Database::open_in_memory()
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
    let database = Database::open_in_memory()
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
    let database = Database::open_in_memory()
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
    let database = Database::open_in_memory()
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
                applied_personality_id: None,
                applied_personality_prompt_hash: None,
                instruction_conversation_id: Some("instruction-thread".to_string()),
                model: AgentModel::Gpt56Sol.as_str().to_string(),
                provider_conversation_id: Some("thread-123".to_string()),
                questions_json: r#"[{"text":"Need tests?"}]"#.to_string(),
                review_comment_resolutions: Vec::new(),
                token_usage_delta: SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: SessionDiffState::Unknown,
                    input_tokens: 3,
                    output_tokens: 5,
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

    // Assert
    assert!(matches!(result, Err(DbError::Query(_))));
    assert_eq!(session.questions.as_deref(), None);
    assert_eq!(session.input_tokens, 0);
    assert_eq!(session.output_tokens, 0);
    assert_eq!(provider_conversation_id.as_deref(), None);
}
