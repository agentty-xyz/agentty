//! Public repository contracts remain available without `test-utils`.

use std::sync::Arc;

use ag_store::Database;
use ag_worker::RunInfo;

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

#[tokio::test]
async fn utility_admission_closure_survives_reopening_the_repository() {
    // Arrange
    let directory = tempfile::tempdir().expect("database directory");
    let path = directory.path().join("agentty.db");
    let database = Database::open(&path).await.expect("database");
    database
        .runs()
        .close_session("retired")
        .await
        .expect("close admission");
    database.pool().close().await;
    // Act
    let reopened = Database::open(&path).await.expect("reopen database");
    let error = reopened
        .runs()
        .create(&RunInfo {
            folder: "repository".into(),
            id: "late".into(),
            parent_id: None,
            project_id: None,
            purpose: "title".into(),
            session_id: Some("retired".into()),
        })
        .await
        .expect_err("late submission must remain closed");
    // Assert
    assert!(error.to_string().contains("closed"));
}

#[tokio::test]
async fn review_checkpoints_are_generation_scoped_and_deleted_with_the_session() {
    // Arrange
    let id = "checkpoint";
    let generation = "generation-a";
    let (database, _project) = pending_review_database().await.expect("pending review");
    let sessions = database.sessions();
    // Act
    sessions
        .begin_review_generation(id, generation, "invocation")
        .await
        .expect("begin");
    sessions
        .save_review_fragment(id, generation, "invocation", "request", "answer")
        .await
        .expect("save");
    sessions
        .save_review_fragment(id, generation, "invocation", "request", "updated")
        .await
        .expect("replace");
    sessions
        .begin_review_generation(id, generation, "invocation")
        .await
        .expect("resume");
    // Assert
    assert_eq!(
        sessions
            .load_review_fragment(id, generation, "request")
            .await
            .expect("load")
            .as_deref(),
        Some("updated")
    );
    assert!(
        sessions
            .load_review_fragment(id, generation, "different")
            .await
            .expect("miss")
            .is_none()
    );
    sessions
        .begin_review_generation(id, "generation-b", "invocation")
        .await
        .expect("supersede");
    sessions
        .save_review_fragment(id, generation, "invocation", "request", "late old answer")
        .await
        .expect("stale write ignored");
    sessions
        .save_review_fragment(id, "generation-b", "invocation", "request", "other")
        .await
        .expect("other generation");

    assert!(
        sessions
            .load_review_fragment(id, generation, "request")
            .await
            .expect("cleared")
            .is_none()
    );
    assert!(
        sessions
            .load_review_fragment(id, "generation-b", "request")
            .await
            .expect("retained")
            .is_some()
    );
    sessions
        .clear_review_fragments(id, "generation-b")
        .await
        .expect("explicit reset");
    sessions
        .save_review_fragment(id, "generation-b", "invocation", "request", "other")
        .await
        .expect("save after reset");
    sessions.delete_session(id).await.expect("delete session");
    assert!(
        sessions
            .load_review_fragment(id, "generation-b", "request")
            .await
            .expect("cascade")
            .is_none()
    );
    sessions
        .begin_review_generation(id, "generation-b", "invocation")
        .await
        .expect("deleted no-op");
    sessions
        .save_review_fragment(id, "generation-b", "invocation", "late", "answer")
        .await
        .expect("deleted session no-op");
    assert!(
        sessions
            .load_review_fragment(id, "generation-b", "late")
            .await
            .expect("late write")
            .is_none()
    );
}

#[tokio::test]
async fn completed_review_and_checkpoint_cleanup_commit_together() {
    // Arrange
    let id = "checkpoint";
    let generation = "generation-a";
    let (database, project) = pending_review_database().await.expect("pending review");
    let sessions = database.sessions();
    sessions
        .begin_review_generation(id, generation, "invocation")
        .await
        .expect("begin");
    sessions
        .save_review_fragment(id, generation, "invocation", "request", "answer")
        .await
        .expect("save");

    // Act / Assert: failure in either write leaves both pending state and
    // evidence intact.
    for rejection in [
        "CREATE TRIGGER reject_completion BEFORE UPDATE OF focused_review_text ON session BEGIN \
         SELECT RAISE(ABORT, 'completion failure'); END",
        "CREATE TRIGGER reject_completion BEFORE DELETE ON session_review_generation BEGIN SELECT \
         RAISE(ABORT, 'completion failure'); END",
    ] {
        sqlx::query(rejection)
            .execute(database.pool())
            .await
            .expect("failure fixture");
        assert!(
            sessions
                .update_session_focused_review(
                    id,
                    Some(ag_session::FocusedReviewStatus::Ready),
                    Some("42".into()),
                    Some("Completed review".into())
                )
                .await
                .is_err()
        );
        let state: String =
            sqlx::query_scalar("SELECT focused_review_status FROM session WHERE id = 'checkpoint'")
                .fetch_one(database.pool())
                .await
                .expect("durable state");
        assert_eq!(state, "Pending");
        assert_eq!(
            sessions
                .load_review_fragment(id, generation, "request")
                .await
                .expect(id),
            Some("answer".into())
        );
        sqlx::query("DROP TRIGGER reject_completion")
            .execute(database.pool())
            .await
            .expect("repair fixture");
    }
    sessions
        .update_session_focused_review(
            id,
            Some(ag_session::FocusedReviewStatus::Partial),
            Some("42".into()),
            Some("Partial review".into()),
        )
        .await
        .expect("partial persistence");
    assert!(
        sessions
            .load_review_fragment(id, generation, "request")
            .await
            .expect("partial checkpoint")
            .is_some()
    );
    sessions
        .update_session_focused_review(
            id,
            Some(ag_session::FocusedReviewStatus::Ready),
            Some("42".into()),
            Some("Completed review".into()),
        )
        .await
        .expect("successful completion");
    sessions
        .save_review_fragment(id, generation, "invocation", "late", "late answer")
        .await
        .expect("late save ignored");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_review_fragment")
        .fetch_one(database.pool())
        .await
        .expect("checkpoint count");
    assert_eq!(count, 0);
    let state = sessions
        .load_session_focused_reviews_for_project(project)
        .await
        .expect("complete state");
    assert_eq!(state[0].text, "Completed review");
    let status: String =
        sqlx::query_scalar("SELECT focused_review_status FROM session WHERE id = 'checkpoint'")
            .fetch_one(database.pool())
            .await
            .expect("terminal state");
    assert_eq!(status, "Ready");
}

/// Creates a pending review for checkpoint lifecycle regression tests.
async fn pending_review_database() -> Result<(Database, i64), ag_store::DbError> {
    let database = Database::open_in_memory().await?;
    let project = database
        .projects()
        .upsert_project("checkpoint-project", None)
        .await?;
    database
        .sessions()
        .insert_session("checkpoint", "model", "main", "Review", project)
        .await?;
    database
        .sessions()
        .update_session_focused_review(
            "checkpoint",
            Some(ag_session::FocusedReviewStatus::Pending),
            Some("42".into()),
            None,
        )
        .await?;

    Ok((database, project))
}

#[tokio::test]
async fn explicit_review_invalidation_fences_reactivated_identical_generations() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("review", "model", "main", "Review", project)
        .await
        .expect("session");
    database
        .sessions()
        .begin_review_generation("review", "same-inputs", "before-rebase")
        .await
        .expect("generation");
    database
        .sessions()
        .save_review_fragment("review", "same-inputs", "before-rebase", "batch", "old")
        .await
        .expect("checkpoint");

    // Act
    database
        .sessions()
        .update_session_focused_review("review", None, None, None)
        .await
        .expect("invalidation");
    database
        .sessions()
        .begin_review_generation("review", "same-inputs", "after-rebase")
        .await
        .expect("reactivation");
    database
        .sessions()
        .save_review_fragment(
            "review",
            "same-inputs",
            "before-rebase",
            "batch",
            "late old result",
        )
        .await
        .expect("fenced write");

    // Assert
    assert_eq!(
        database
            .sessions()
            .load_review_fragment("review", "same-inputs", "batch")
            .await
            .expect("no stale checkpoint"),
        None
    );
    database
        .sessions()
        .save_review_fragment("review", "same-inputs", "after-rebase", "batch", "new")
        .await
        .expect("current write");
    database
        .sessions()
        .begin_review_generation("review", "same-inputs", "retry")
        .await
        .expect("retry admission");
    assert_eq!(
        database
            .sessions()
            .load_review_fragment("review", "same-inputs", "batch")
            .await
            .expect("reuse current evidence"),
        Some("new".into())
    );
}
