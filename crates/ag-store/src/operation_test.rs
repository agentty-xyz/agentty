use crate::{AppRepositories, DbError};

#[tokio::test]
/// Claims new and failed idempotent operations while leaving accepted
/// operations untouched.
async fn test_claim_session_operation_recovers_only_terminal_failures() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/operation-project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");

    // Act
    let first_claim = database
        .operations()
        .claim_session_operation("rollup-1", "session-a", "reply")
        .await
        .expect("failed to claim new operation");
    let queued_claim = database
        .operations()
        .claim_session_operation("rollup-1", "session-a", "reply")
        .await
        .expect("failed to inspect queued operation");
    database
        .operations()
        .mark_session_operation_failed("rollup-1", "restart")
        .await
        .expect("failed to mark operation failed");
    let recovered_claim = database
        .operations()
        .claim_session_operation("rollup-1", "session-a", "reply")
        .await
        .expect("failed to reclaim failed operation");
    database
        .operations()
        .mark_session_operation_done("rollup-1")
        .await
        .expect("failed to mark operation done");
    let done_claim = database
        .operations()
        .claim_session_operation("rollup-1", "session-a", "reply")
        .await
        .expect("failed to inspect completed operation");

    // Assert
    assert!(first_claim);
    assert!(!queued_claim);
    assert!(recovered_claim);
    assert!(!done_claim);
}

#[tokio::test]
async fn recovery_failure_reports_semantic_operation_context() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    sqlx::query("DROP TABLE session_operation")
        .execute(&pool)
        .await
        .expect("failed to drop operation table");

    // Act
    let error = database
        .operations()
        .fail_unfinished_session_operations("restart")
        .await
        .expect_err("recovery should fail without its table");

    // Assert
    assert!(matches!(
        error,
        DbError::QueryContext {
            operation: "fail unfinished session operations",
            ..
        }
    ));
}
