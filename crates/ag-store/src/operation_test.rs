use crate::{AppRepositories, DbError};

#[tokio::test]
async fn terminal_updates_atomically_honor_prior_cancellation_requests() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("database");
    let project = database
        .projects()
        .upsert_project("operation-project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "gpt-5.6-sol", "main", "Review", project)
        .await
        .expect("session");

    for requested in [false, true] {
        for failed in [false, true] {
            let id = format!("operation-{requested}-{failed}");
            database
                .operations()
                .insert_session_operation(&id, "session", "rebase")
                .await
                .expect("queue");
            database
                .operations()
                .mark_session_operation_running(&id)
                .await
                .expect("start");

            // Act
            if requested {
                database
                    .operations()
                    .request_cancel_for_session_operations("session")
                    .await
                    .expect("cancel");
            }
            if failed {
                database
                    .operations()
                    .mark_session_operation_failed(&id, "workflow failed")
                    .await
                    .expect("settle failure");
            } else {
                database
                    .operations()
                    .mark_session_operation_done(&id)
                    .await
                    .expect("settle success");
            }
            // A request that arrives after settlement must not change its
            // result.
            database
                .operations()
                .request_cancel_for_session_operations("session")
                .await
                .expect("late cancel");
            let row: (String, Option<String>, bool, Option<i64>, Option<i64>) = sqlx::query_as(
                "SELECT status, last_error, cancel_requested, finished_at, heartbeat_at FROM \
                 session_operation WHERE id = ?",
            )
            .bind(&id)
            .fetch_one(&pool)
            .await
            .expect("terminal row");

            // Assert
            let status = if requested {
                "canceled"
            } else if failed {
                "failed"
            } else {
                "done"
            };
            let reason = if failed {
                Some("workflow failed")
            } else if requested {
                Some("Canceled by user")
            } else {
                None
            };
            assert_eq!(row.0, status);
            assert_eq!(row.1.as_deref(), reason);
            assert_eq!(row.2, requested);
            assert!(row.3.is_some());
            assert_eq!(row.3, row.4);
            assert!(
                !database
                    .operations()
                    .is_session_operation_unfinished(&id)
                    .await
                    .expect("finished")
            );
        }
    }
}

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

#[tokio::test]
async fn heartbeat_updates_running_operations_without_reviving_terminal_rows() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("database");
    let project = database
        .projects()
        .upsert_project("/tmp/heartbeat-project", Some("main".into()))
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "gpt-5.6-sol", "main", "Review", project)
        .await
        .expect("session");
    database
        .operations()
        .insert_session_operation("run", "session", "reply")
        .await
        .expect("operation");
    // Act
    database
        .operations()
        .heartbeat("run")
        .await
        .expect("queued heartbeat");
    let queued = database
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("queued");
    database
        .operations()
        .mark_session_operation_running("run")
        .await
        .expect("start");
    sqlx::query("UPDATE session_operation SET heartbeat_at = 0 WHERE id = 'run'")
        .execute(&pool)
        .await
        .expect("old heartbeat");
    database
        .operations()
        .heartbeat("run")
        .await
        .expect("running heartbeat");
    let running = database
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("running");
    database
        .operations()
        .mark_session_operation_done("run")
        .await
        .expect("done");
    sqlx::query("UPDATE session_operation SET heartbeat_at = 0 WHERE id = 'run'")
        .execute(&pool)
        .await
        .expect("terminal heartbeat");
    database
        .operations()
        .heartbeat("run")
        .await
        .expect("terminal heartbeat ignored");
    let terminal: (String, i64) =
        sqlx::query_as("SELECT status, heartbeat_at FROM session_operation WHERE id = 'run'")
            .fetch_one(&pool)
            .await
            .expect("terminal row");
    // Assert
    assert_eq!(queued[0].heartbeat_at, None);
    assert!(running[0].heartbeat_at.is_some_and(|value| value > 0));
    assert_eq!(terminal, ("done".into(), 0));
}
