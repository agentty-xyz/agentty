use ag_agent::{AgentModel, SessionStats};
use tempfile::tempdir;

use super::support::{insert_session_fixture, load_session_row};
use crate::connection::Database;
use crate::error::DbError;
use crate::{NewSessionReviewCommentResolution, SessionOperationRow, SessionTurnMetadata};

/// Verifies `load_unfinished_session_operations()` returns only queued and
/// running rows.
#[tokio::test]
async fn test_load_unfinished_session_operations_returns_only_queued_and_running_rows() {
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
    database
        .operations()
        .insert_session_operation("operation-queued", "session-a", "merge")
        .await
        .expect("failed to insert queued operation");
    database
        .operations()
        .insert_session_operation("operation-running", "session-a", "sync")
        .await
        .expect("failed to insert running operation");
    database
        .operations()
        .insert_session_operation("operation-done", "session-a", "review")
        .await
        .expect("failed to insert done operation");
    database
        .operations()
        .mark_session_operation_running("operation-running")
        .await
        .expect("failed to mark running operation");
    database
        .operations()
        .mark_session_operation_running("operation-done")
        .await
        .expect("failed to mark done operation running");
    database
        .operations()
        .mark_session_operation_done("operation-done")
        .await
        .expect("failed to mark done operation");

    // Act
    let unfinished_rows = database
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("failed to load unfinished operations");

    // Assert
    assert_eq!(unfinished_rows.len(), 2);
    assert_eq!(unfinished_rows[0].id, "operation-queued");
    assert_eq!(unfinished_rows[0].status, "queued");
    assert_eq!(unfinished_rows[1].id, "operation-running");
    assert_eq!(unfinished_rows[1].status, "running");
}

/// Verifies `request_cancel_for_session_operations()` marks only
/// unfinished rows.
#[tokio::test]
async fn test_request_cancel_for_session_operations_marks_only_unfinished_rows() {
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
    database
        .operations()
        .insert_session_operation("operation-queued", "session-a", "merge")
        .await
        .expect("failed to insert queued operation");
    database
        .operations()
        .insert_session_operation("operation-done", "session-a", "review")
        .await
        .expect("failed to insert done operation");
    database
        .operations()
        .mark_session_operation_running("operation-done")
        .await
        .expect("failed to mark done operation running");
    database
        .operations()
        .mark_session_operation_done("operation-done")
        .await
        .expect("failed to mark done operation");

    // Act
    database
        .operations()
        .request_cancel_for_session_operations("session-a")
        .await
        .expect("failed to request cancel");
    let queued_row = load_session_operation_row(&database, "operation-queued").await;
    let done_row = load_session_operation_row(&database, "operation-done").await;

    // Assert
    assert!(queued_row.cancel_requested);
    assert!(!done_row.cancel_requested);
}

/// Verifies `is_session_operation_unfinished()` returns `false` for a
/// completed operation.
#[tokio::test]
async fn test_is_session_operation_unfinished_returns_false_for_done_operation() {
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
    database
        .operations()
        .insert_session_operation("operation-a", "session-a", "merge")
        .await
        .expect("failed to insert operation");
    database
        .operations()
        .mark_session_operation_running("operation-a")
        .await
        .expect("failed to mark operation running");
    database
        .operations()
        .mark_session_operation_done("operation-a")
        .await
        .expect("failed to mark operation done");

    // Act
    let is_unfinished = database
        .operations()
        .is_session_operation_unfinished("operation-a")
        .await
        .expect("failed to check unfinished operation state");

    // Assert
    assert!(!is_unfinished);
}

/// Verifies `is_cancel_requested_for_operation()` returns `true` for a
/// cancelled operation and `false` for an unaffected one.
#[tokio::test]
async fn test_is_cancel_requested_for_operation_scoped_to_single_operation() {
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
    database
        .operations()
        .insert_session_operation("operation-cancelled", "session-a", "reply")
        .await
        .expect("failed to insert cancelled operation");
    database
        .operations()
        .insert_session_operation("operation-new", "session-a", "reply")
        .await
        .expect("failed to insert new operation");

    // Cancel only the first operation via session-level bulk update.
    database
        .operations()
        .request_cancel_for_session_operations("session-a")
        .await
        .expect("failed to request cancel");

    // Simulate a new operation created after the cancel request by
    // resetting its flag directly (mirrors real flow where new
    // operations are inserted with cancel_requested = 0 by default).
    sqlx::query!("UPDATE session_operation SET cancel_requested = 0 WHERE id = 'operation-new'")
        .execute(&database.pool)
        .await
        .expect("failed to reset new operation flag");

    // Act
    let cancelled_flag = database
        .operations()
        .is_cancel_requested_for_operation("operation-cancelled")
        .await
        .expect("failed to check cancelled operation");
    let new_flag = database
        .operations()
        .is_cancel_requested_for_operation("operation-new")
        .await
        .expect("failed to check new operation");

    // Assert — only the cancelled operation is flagged; the new one
    // proceeds normally.
    assert!(cancelled_flag);
    assert!(!new_flag);
}

/// Verifies `mark_session_operation_running()` sets the running state and
/// timestamps.
#[tokio::test]
async fn test_mark_session_operation_running_sets_started_at_and_heartbeat() {
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
    database
        .operations()
        .insert_session_operation("operation-a", "session-a", "merge")
        .await
        .expect("failed to insert operation");

    // Act
    database
        .operations()
        .mark_session_operation_running("operation-a")
        .await
        .expect("failed to mark operation running");
    let running_row = load_session_operation_row(&database, "operation-a").await;

    // Assert
    assert_eq!(running_row.status, "running");
    assert!(running_row.started_at.is_some());
    assert!(running_row.heartbeat_at.is_some());
    assert_eq!(running_row.last_error, None);
}

/// Verifies `mark_session_operation_done()` sets the terminal completion
/// fields.
#[tokio::test]
async fn test_mark_session_operation_done_sets_finished_state() {
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
    database
        .operations()
        .insert_session_operation("operation-a", "session-a", "merge")
        .await
        .expect("failed to insert operation");
    database
        .operations()
        .mark_session_operation_running("operation-a")
        .await
        .expect("failed to mark operation running");

    // Act
    database
        .operations()
        .mark_session_operation_done("operation-a")
        .await
        .expect("failed to mark operation done");
    let done_row = load_session_operation_row(&database, "operation-a").await;

    // Assert
    assert_eq!(done_row.status, "done");
    assert!(done_row.finished_at.is_some());
    assert!(done_row.heartbeat_at.is_some());
    assert_eq!(done_row.last_error, None);
}

#[tokio::test]
/// Verifies a failed review-operation insert cannot leave a completed turn
/// behind after the database is reopened.
async fn test_persist_session_turn_metadata_and_review_operation_are_restart_atomic() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp directory");
    let db_path = temp_dir.path().join("agentty.db");
    let database = Database::open(&db_path)
        .await
        .expect("failed to open database");
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
    let turn_metadata = |resolution: &str| SessionTurnMetadata {
        applied_personality_id: None,
        applied_personality_prompt_hash: None,
        instruction_conversation_id: None,
        model: AgentModel::Gpt56Sol.as_str().to_string(),
        provider_conversation_id: Some("thread-123".to_string()),
        questions_json: r#"[{"text":"Need tests?"}]"#.to_string(),
        review_comment_resolutions: vec![NewSessionReviewCommentResolution {
            commit_hash: None,
            reply: "Applied the validation.".to_string(),
            reply_token: "token-1".to_string(),
            resolution: resolution.to_string(),
            review_request_display_id: "#42".to_string(),
            thread_id: "thread-1".to_string(),
        }],
        token_usage_delta: SessionStats::default(),
    };

    // Act
    let result = database
        .sessions()
        .persist_session_turn_metadata("session-a", &turn_metadata("invalid"))
        .await;
    database.pool().close().await;
    let database = Database::open(&db_path)
        .await
        .expect("failed to reopen database");
    let session = load_session_row(&database, "session-a").await;
    let operations = database
        .reviews()
        .load_session_review_comment_resolutions("session-a")
        .await
        .expect("failed to load review operations");
    let provider_conversation_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load provider conversation id");

    // Assert
    assert!(matches!(result, Err(DbError::Query(_))));
    assert_eq!(session.questions, None);
    assert_eq!(provider_conversation_id, None);
    assert_eq!(operations, Vec::new());

    // Act
    database
        .sessions()
        .persist_session_turn_metadata("session-a", &turn_metadata("fixed"))
        .await
        .expect("failed to persist completed turn and review operation");
    database.pool().close().await;
    let database = Database::open(&db_path)
        .await
        .expect("failed to reopen database after retry");
    let session = load_session_row(&database, "session-a").await;
    let operations = database
        .reviews()
        .load_session_review_comment_resolutions("session-a")
        .await
        .expect("failed to load persisted review operations");
    let provider_conversation_id = database
        .sessions()
        .get_session_provider_conversation_id("session-a")
        .await
        .expect("failed to load provider conversation id");

    // Assert
    assert_eq!(
        session.questions.as_deref(),
        Some(r#"[{"text":"Need tests?"}]"#)
    );
    assert_eq!(provider_conversation_id.as_deref(), Some("thread-123"));
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].reply, "Applied the validation.");
    assert_eq!(operations[0].resolution, "fixed");
}

/// Loads one persisted session-operation row regardless of lifecycle
/// status.
async fn load_session_operation_row(
    database: &Database,
    operation_id: &str,
) -> SessionOperationRow {
    sqlx::query_as!(
        SessionOperationRow,
        r#"
SELECT id AS "id!", session_id AS "session_id!", kind AS "kind!", status AS "status!",
       queued_at, started_at, finished_at,
       heartbeat_at, last_error, cancel_requested AS "cancel_requested: _"
FROM session_operation
WHERE id = ?
"#,
        operation_id
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load session operation row")
}
