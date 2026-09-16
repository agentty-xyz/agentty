//! Public repository contracts remain available without `test-utils`.

use std::sync::Arc;

use ag_store::Database;

#[tokio::test]
async fn worker_settlement_preserves_in_flight_cancellation_for_workflow_commands() {
    // Arrange
    let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 123))
        .await
        .expect("database");
    let project = database
        .projects()
        .upsert_project("workflow-project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "gpt-5.6-sol", "main", "Review", project)
        .await
        .expect("session");

    for kind in ["create_review_request", "rebase"] {
        for failed in [false, true] {
            let id = format!("{kind}-{failed}");
            database
                .operations()
                .insert_session_operation(&id, "session", kind)
                .await
                .expect("queue");
            database
                .operations()
                .mark_session_operation_running(&id)
                .await
                .expect("start");
            let (errors, mut observed_errors) = tokio::sync::mpsc::unbounded_channel();

            // Act: cancellation arrives after execution starts. Neither result
            // is a typed cancellation error recognized by the host predicate.
            let result = ag_worker::execute(
                database.operations(),
                &ag_worker::HeartbeatClock,
                &id,
                async {
                    database
                        .operations()
                        .request_cancel_for_session_operations("session")
                        .await
                        .expect("cancel during execution");
                    if failed {
                        Err("workflow failed")
                    } else {
                        Ok(())
                    }
                },
                |_| false,
                |error| {
                    let _ = errors.send(error);
                },
            )
            .await;
            let row: (String, bool, i64) = sqlx::query_as(
                "SELECT status, cancel_requested, finished_at FROM session_operation WHERE id = ?",
            )
            .bind(&id)
            .fetch_one(database.pool())
            .await
            .expect("terminal row");

            // Assert
            assert_eq!(
                result,
                if failed {
                    Err("workflow failed")
                } else {
                    Ok(())
                }
            );
            assert_eq!(row, ("canceled".into(), true, 123));
            assert!(observed_errors.try_recv().is_err());
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
async fn maintenance_operations_use_the_same_public_repository_contract() {
    // Arrange
    let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 123))
        .await
        .expect("database should open");
    let project_id = database
        .projects()
        .upsert_project("repository-contract", None)
        .await
        .expect("project should persist");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Draft", project_id)
        .await
        .expect("session should persist");

    // Act
    database
        .sessions()
        .update_session_created_at("session-a", 100)
        .await
        .expect("creation timestamp should update");
    database
        .sessions()
        .update_session_updated_at("session-a", 200)
        .await
        .expect("modification timestamp should update");
    database
        .activity()
        .clear_session_activity()
        .await
        .expect("activity should clear");
    database
        .activity()
        .backfill_session_activity_from_sessions()
        .await
        .expect("activity should rebuild from session timestamps");
    let sessions = database.sessions().load_sessions().await.expect("sessions");
    let activity = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("activity timestamps");

    // Assert
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].created_at, 100);
    assert_eq!(sessions[0].updated_at, 200);
    assert_eq!(activity, vec![100]);
}
