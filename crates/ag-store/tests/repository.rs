//! Public repository contracts remain available without `test-utils`.

use std::sync::Arc;

use ag_session::AgentSelectionMetadata;
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

    // Act / Assert: failed writes preserve the pending review and evidence.
    for rejection in [
        "CREATE TRIGGER reject_completion BEFORE UPDATE OF focused_review_text ON session BEGIN \
         SELECT RAISE(ABORT, 'completion failure'); END",
        "CREATE TRIGGER reject_completion BEFORE DELETE ON session_review_generation BEGIN SELECT \
         RAISE(ABORT, 'completion failure'); END",
        "CREATE TRIGGER reject_completion BEFORE INSERT ON session_review_audit BEGIN SELECT \
         RAISE(ABORT, 'archive failure'); END",
    ] {
        assert_completion_failure_is_atomic(&database, id, generation, rejection)
            .await
            .expect("atomic failure");
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
    let audit = sessions
        .load_completed_review_audit(id)
        .await
        .expect("completed audit");
    assert_eq!(
        audit,
        [ag_store::SessionReviewAuditRow {
            answer: "answer".into(),
            generation: generation.into(),
            request: "request".into(),
        }]
    );
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
async fn generated_review_write_is_fenced_by_its_active_invocation() {
    // Arrange
    let (database, project) = pending_review_database().await.expect("pending review");
    let sessions = database.sessions();
    sessions
        .begin_review_generation("checkpoint", "same-inputs", "first")
        .await
        .expect("first generation");

    // Act: a partial result is accepted, then invalidation closes its
    // invocation.
    let partial = sessions
        .update_session_focused_review_for_generation(
            "checkpoint",
            "first",
            ag_session::FocusedReviewStatus::Partial,
            Some("42".into()),
            Some("Partial review".into()),
        )
        .await
        .expect("partial result");
    sessions
        .update_session_focused_review("checkpoint", None, None, None)
        .await
        .expect("invalidation");
    let late = sessions
        .update_session_focused_review_for_generation(
            "checkpoint",
            "first",
            ag_session::FocusedReviewStatus::Ready,
            Some("42".into()),
            Some("Stale review".into()),
        )
        .await
        .expect("late result is ignored");
    sessions
        .update_session_focused_review(
            "checkpoint",
            Some(ag_session::FocusedReviewStatus::Pending),
            Some("42".into()),
            None,
        )
        .await
        .expect("next pending review");
    sessions
        .begin_review_generation("checkpoint", "same-inputs", "second")
        .await
        .expect("replacement generation");
    let replaced = sessions
        .update_session_focused_review_for_generation(
            "checkpoint",
            "first",
            ag_session::FocusedReviewStatus::Ready,
            Some("42".into()),
            Some("Stale review".into()),
        )
        .await
        .expect("replaced result is ignored");
    let current = sessions
        .update_session_focused_review_for_generation(
            "checkpoint",
            "second",
            ag_session::FocusedReviewStatus::Ready,
            Some("42".into()),
            Some("Current review".into()),
        )
        .await
        .expect("current result");

    // Assert
    assert!(partial);
    assert!(!late);
    assert!(!replaced);
    assert!(current);
    let reviews = sessions
        .load_session_focused_reviews_for_project(project)
        .await
        .expect("saved review");
    assert_eq!(reviews[0].text, "Current review");
}

#[tokio::test]
async fn generated_review_completion_rolls_back_when_checkpoint_cleanup_fails() {
    // Arrange
    let (database, project) = pending_review_database().await.expect("pending review");
    let sessions = database.sessions();
    sessions
        .begin_review_generation("checkpoint", "same-inputs", "active")
        .await
        .expect("active generation");
    sqlx::query(
        "CREATE TRIGGER reject_generation_cleanup BEFORE DELETE ON session_review_generation \
         BEGIN SELECT RAISE(ABORT, 'cleanup failed'); END",
    )
    .execute(database.pool())
    .await
    .expect("failure fixture");

    // Act
    let result = sessions
        .update_session_focused_review_for_generation(
            "checkpoint",
            "active",
            ag_session::FocusedReviewStatus::Ready,
            Some("42".into()),
            Some("Uncommitted review".into()),
        )
        .await;

    // Assert
    assert!(result.is_err());
    assert_eq!(
        sessions
            .load_session_focused_reviews_for_project(project)
            .await
            .expect("no committed review"),
        [] as [ag_store::SessionFocusedReviewRow; 0]
    );
    sessions
        .save_review_fragment("checkpoint", "same-inputs", "active", "batch", "evidence")
        .await
        .expect("generation remains active");
    assert_eq!(
        sessions
            .load_review_fragment("checkpoint", "same-inputs", "batch")
            .await
            .expect("checkpoint"),
        Some("evidence".into())
    );
}

#[tokio::test]
async fn completed_review_audit_survives_invalidation_partial_work_and_stale_completion() {
    // Arrange
    let database = completed_review_audit_database()
        .await
        .expect("completed review");
    let sessions = database.sessions();
    let old_audit = sessions
        .load_completed_review_audit("checkpoint")
        .await
        .expect("old audit");
    assert_eq!(old_audit.len(), 1);

    // Act / Assert
    sessions
        .update_session_focused_review("checkpoint", None, None, None)
        .await
        .expect("invalidation");
    assert_eq!(
        sessions
            .load_completed_review_audit("checkpoint")
            .await
            .expect("retained audit"),
        old_audit
    );
    begin_partial_review_replacement(&database)
        .await
        .expect("partial replacement");
    assert!(
        !sessions
            .update_session_focused_review_for_generation(
                "checkpoint",
                "old",
                ag_session::FocusedReviewStatus::Ready,
                Some("41".into()),
                Some("Stale review".into()),
            )
            .await
            .expect("stale completion")
    );
    assert_eq!(
        sessions
            .load_completed_review_audit("checkpoint")
            .await
            .expect("retained audit"),
        old_audit
    );
}

#[tokio::test]
async fn restoring_cached_ready_reviews_preserves_the_completed_audit() {
    // Arrange
    let database = completed_review_audit_database()
        .await
        .expect("completed review");
    let sessions = database.sessions();
    let old_audit = sessions
        .load_completed_review_audit("checkpoint")
        .await
        .expect("old audit");
    assert_eq!(old_audit.len(), 1);

    // Act / Assert: both repeated Ready persistence and invalidation followed
    // by cache restoration lack a new generation.
    for invalidate in [false, true] {
        if invalidate {
            sessions
                .update_session_focused_review("checkpoint", None, None, None)
                .await
                .expect("invalidation");
        }
        sessions
            .update_session_focused_review(
                "checkpoint",
                Some(ag_session::FocusedReviewStatus::Ready),
                Some("41".into()),
                Some("Old review".into()),
            )
            .await
            .expect("cache restoration");
        assert_eq!(
            sessions
                .load_completed_review_audit("checkpoint")
                .await
                .expect("retained audit"),
            old_audit
        );
    }
}

#[tokio::test]
async fn completing_an_active_generation_without_fragments_replaces_the_previous_audit() {
    // Arrange
    let database = completed_review_audit_database()
        .await
        .expect("completed review");
    let sessions = database.sessions();
    assert_eq!(
        sessions
            .load_completed_review_audit("checkpoint")
            .await
            .expect("old audit")
            .len(),
        1
    );
    sessions
        .begin_review_generation("checkpoint", "new-inputs", "current")
        .await
        .expect("new generation");

    // Act
    assert!(
        sessions
            .update_session_focused_review_for_generation(
                "checkpoint",
                "current",
                ag_session::FocusedReviewStatus::Ready,
                Some("42".into()),
                Some("New review".into()),
            )
            .await
            .expect("new completion")
    );

    // Assert
    assert_eq!(
        sessions
            .load_completed_review_audit("checkpoint")
            .await
            .expect("replaced audit"),
        []
    );
}

#[tokio::test]
async fn completed_review_audit_replacement_rolls_back_on_archival_or_cleanup_failure() {
    // Arrange
    let database = completed_review_audit_database()
        .await
        .expect("completed review");
    let old_audit = database
        .sessions()
        .load_completed_review_audit("checkpoint")
        .await
        .expect("old audit");
    begin_partial_review_replacement(&database)
        .await
        .expect("partial replacement");

    // Act / Assert
    for rejection in [
        "CREATE TRIGGER reject_audit BEFORE DELETE ON session_review_audit BEGIN SELECT \
         RAISE(ABORT, 'archive failure'); END",
        "CREATE TRIGGER reject_audit BEFORE INSERT ON session_review_audit BEGIN SELECT \
         RAISE(ABORT, 'archive failure'); END",
        "CREATE TRIGGER reject_audit BEFORE DELETE ON session_review_generation BEGIN SELECT \
         RAISE(ABORT, 'cleanup failure'); END",
    ] {
        assert_audit_replacement_failure_is_atomic(&database, &old_audit, rejection)
            .await
            .expect("atomic replacement");
    }
}

#[tokio::test]
async fn completed_review_audit_is_replaced_on_completion_and_removed_with_its_session() {
    // Arrange
    let database = completed_review_audit_database()
        .await
        .expect("completed review");
    let sessions = database.sessions();
    begin_partial_review_replacement(&database)
        .await
        .expect("partial replacement");

    // Act
    let completed = sessions
        .update_session_focused_review_for_generation(
            "checkpoint",
            "current",
            ag_session::FocusedReviewStatus::Ready,
            Some("42".into()),
            Some("New review".into()),
        )
        .await
        .expect("new completion");

    // Assert
    assert!(completed);
    let audit = sessions
        .load_completed_review_audit("checkpoint")
        .await
        .expect("new audit");
    assert_eq!(
        audit,
        [ag_store::SessionReviewAuditRow {
            answer: "new candidate".into(),
            generation: "new-inputs".into(),
            request: "new discovery".into(),
        }]
    );
    assert_eq!(
        sessions
            .load_review_fragment("checkpoint", "new-inputs", "new discovery")
            .await
            .expect("closed checkpoint"),
        None
    );
    sessions
        .save_review_fragment(
            "checkpoint",
            "new-inputs",
            "current",
            "late",
            "late evidence",
        )
        .await
        .expect("late save ignored");
    assert_eq!(
        sessions
            .load_completed_review_audit("checkpoint")
            .await
            .expect("unchanged audit"),
        audit
    );
    sessions
        .delete_session("checkpoint")
        .await
        .expect("delete session");
    assert_eq!(
        sessions
            .load_completed_review_audit("checkpoint")
            .await
            .expect("cascaded audit"),
        []
    );
}

/// Seeds a completed audit through the same public persistence boundary as its
/// consumer.
async fn completed_review_audit_database() -> Result<Database, ag_store::DbError> {
    let (database, _) = pending_review_database().await?;
    let sessions = database.sessions();
    sessions
        .begin_review_generation("checkpoint", "old-inputs", "old")
        .await?;
    sessions
        .save_review_fragment(
            "checkpoint",
            "old-inputs",
            "old",
            "old discovery",
            "old candidate",
        )
        .await?;
    assert!(
        sessions
            .update_session_focused_review_for_generation(
                "checkpoint",
                "old",
                ag_session::FocusedReviewStatus::Ready,
                Some("41".into()),
                Some("Old review".into()),
            )
            .await?
    );

    Ok(database)
}

/// Stages resumable replacement evidence without overwriting the completed
/// audit.
async fn begin_partial_review_replacement(database: &Database) -> Result<(), ag_store::DbError> {
    let sessions = database.sessions();
    sessions
        .begin_review_generation("checkpoint", "new-inputs", "current")
        .await?;
    sessions
        .save_review_fragment(
            "checkpoint",
            "new-inputs",
            "current",
            "new discovery",
            "new candidate",
        )
        .await?;
    assert!(
        sessions
            .update_session_focused_review_for_generation(
                "checkpoint",
                "current",
                ag_session::FocusedReviewStatus::Partial,
                Some("42".into()),
                Some("Partial review".into()),
            )
            .await?
    );

    Ok(())
}

/// Checks both durable results and retry state after a failed replacement
/// transaction.
async fn assert_audit_replacement_failure_is_atomic(
    database: &Database,
    old_audit: &[ag_store::SessionReviewAuditRow],
    rejection: &'static str,
) -> Result<(), ag_store::DbError> {
    let sessions = database.sessions();
    sqlx::query(rejection).execute(database.pool()).await?;
    assert!(
        sessions
            .update_session_focused_review_for_generation(
                "checkpoint",
                "current",
                ag_session::FocusedReviewStatus::Ready,
                Some("42".into()),
                Some("New review".into()),
            )
            .await
            .is_err()
    );
    assert_eq!(
        sessions.load_completed_review_audit("checkpoint").await?,
        old_audit
    );
    assert_eq!(
        sessions
            .load_review_fragment("checkpoint", "new-inputs", "new discovery")
            .await?,
        Some("new candidate".into())
    );
    let state: (String, String) = sqlx::query_as(
        "SELECT focused_review_status, focused_review_text FROM session WHERE id = 'checkpoint'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(state, ("Partial".into(), "Partial review".into()));
    sqlx::query("DROP TRIGGER reject_audit")
        .execute(database.pool())
        .await?;

    Ok(())
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

#[tokio::test]
async fn model_switch_rolls_back_selection_conversations_and_defaults_on_any_write_failure() {
    // Arrange
    let (database, project) = model_switch_database().await.expect("model switch fixture");
    let sessions = database.sessions();
    let selection = ag_session::AgentSelection::new(
        ag_session::AgentKind::Claude,
        ag_session::AgentModel::ClaudeOpus55,
    );
    let original: (String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT agent, model, provider_conversation_id, \
         app_server_instruction_provider_conversation_id FROM session WHERE id = 'switch'",
    )
    .fetch_one(database.pool())
    .await
    .expect("original");

    // Act / Assert: failures at early and late writes leave all state
    // unchanged.
    for rejection in [
        "CREATE TRIGGER reject_switch BEFORE UPDATE OF agent ON session BEGIN SELECT RAISE(ABORT, \
         'save failed'); END",
        "CREATE TRIGGER reject_switch BEFORE UPDATE OF provider_conversation_id ON session BEGIN \
         SELECT RAISE(ABORT, 'save failed'); END",
        "CREATE TRIGGER reject_switch BEFORE UPDATE OF \
         app_server_instruction_provider_conversation_id ON session BEGIN SELECT RAISE(ABORT, \
         'save failed'); END",
        "CREATE TRIGGER reject_switch BEFORE INSERT ON project_setting WHEN NEW.name = \
         'DefaultSmartModel' BEGIN SELECT RAISE(ABORT, 'save failed'); END",
    ] {
        sqlx::query(rejection)
            .execute(database.pool())
            .await
            .expect("failure fixture");
        assert!(
            sessions
                .apply_session_agent_model("switch", selection, true, Some(project))
                .await
                .is_err()
        );
        let actual: (String, String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT agent, model, provider_conversation_id, \
             app_server_instruction_provider_conversation_id FROM session WHERE id = 'switch'",
        )
        .fetch_one(database.pool())
        .await
        .expect("rollback");
        assert_eq!(actual, original);
        assert!(
            database
                .settings()
                .get_project_setting(project, ag_session::SettingName::DefaultSmartAgent)
                .await
                .expect("default")
                .is_none()
        );
        assert!(
            database
                .settings()
                .get_project_setting(project, ag_session::SettingName::DefaultSmartModel)
                .await
                .expect("default")
                .is_none()
        );
        sqlx::query("DROP TRIGGER reject_switch")
            .execute(database.pool())
            .await
            .expect("repair fixture");
    }
}

#[tokio::test]
async fn model_switch_commits_optional_conversation_reset_and_project_defaults() {
    // Arrange
    let (database, project) = model_switch_database().await.expect("model switch fixture");
    let sessions = database.sessions();
    let selection = ag_session::AgentSelection::new(
        ag_session::AgentKind::Claude,
        ag_session::AgentModel::ClaudeOpus55,
    );

    // Act / Assert
    sessions
        .apply_session_agent_model("switch", selection, false, None)
        .await
        .expect("preserve conversation");
    assert_eq!(
        sessions
            .get_session_provider_conversation_id("switch")
            .await
            .expect("conversation"),
        Some("conversation".into())
    );
    sessions
        .apply_session_agent_model("switch", selection, true, Some(project))
        .await
        .expect("commit switch");
    let actual: (String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT agent, model, provider_conversation_id, \
         app_server_instruction_provider_conversation_id FROM session WHERE id = 'switch'",
    )
    .fetch_one(database.pool())
    .await
    .expect("committed");
    assert_eq!(
        actual,
        (
            selection.kind().to_string(),
            selection.model().as_str().into(),
            None,
            None
        )
    );
    assert_eq!(
        database
            .settings()
            .get_project_setting(project, ag_session::SettingName::DefaultSmartAgent)
            .await
            .expect("default"),
        Some(selection.kind().name().into())
    );
    assert_eq!(
        database
            .settings()
            .get_project_setting(project, ag_session::SettingName::DefaultSmartModel)
            .await
            .expect("default"),
        Some(selection.model().as_str().into())
    );
}

/// Persistent session state shared by model-switch transaction regressions.
async fn model_switch_database() -> Result<(Database, i64), ag_store::DbError> {
    let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 123)).await?;
    let project = database
        .projects()
        .upsert_project("model-switch", None)
        .await?;
    database
        .sessions()
        .insert_session("switch", "gpt-5.6-sol", "main", "Review", project)
        .await?;
    database
        .sessions()
        .update_session_provider_conversation_id("switch", Some("conversation".into()))
        .await?;
    database
        .sessions()
        .update_session_instruction_conversation_id("switch", Some("instructions".into()))
        .await?;

    Ok((database, project))
}

/// Exercises failed review, archival, and cleanup writes through the public
/// boundary.
async fn assert_completion_failure_is_atomic(
    database: &Database,
    id: &str,
    generation: &str,
    rejection: &'static str,
) -> Result<(), ag_store::DbError> {
    let sessions = database.sessions();
    sqlx::query(rejection).execute(database.pool()).await?;
    assert!(
        sessions
            .update_session_focused_review(
                id,
                Some(ag_session::FocusedReviewStatus::Ready),
                Some("42".into()),
                Some("Completed review".into()),
            )
            .await
            .is_err()
    );
    let state: String =
        sqlx::query_scalar("SELECT focused_review_status FROM session WHERE id = ?")
            .bind(id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(state, "Pending");
    assert_eq!(
        sessions.load_completed_review_audit(id).await?,
        Vec::<ag_store::SessionReviewAuditRow>::new()
    );
    assert_eq!(
        sessions
            .load_review_fragment(id, generation, "request")
            .await?,
        Some("answer".into())
    );
    sqlx::query("DROP TRIGGER reject_completion")
        .execute(database.pool())
        .await?;

    Ok(())
}

#[tokio::test]
async fn session_archive_continuation_counts_only_omitted_rows() {
    for limit in [0, 10] {
        for has_omitted_archive in [false, true] {
            // Arrange
            let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 123))
                .await
                .expect("database");
            let project = database
                .projects()
                .upsert_project("archive-project", None)
                .await
                .expect("project");
            let visible_count = limit + 1;
            let archive_count = visible_count + usize::from(has_omitted_archive);
            for index in 0..archive_count {
                let id = format!("archive-{index:02}");
                database
                    .sessions()
                    .insert_session(&id, "gpt-5.6-sol", "main", "Done", project)
                    .await
                    .expect("archive");
                database
                    .sessions()
                    .update_session_updated_at(&id, 100 - i64::try_from(index).expect("timestamp"))
                    .await
                    .expect("timestamp");
            }
            let pinned_id = format!("archive-{limit:02}");

            // Act
            let (rows, has_more) = database
                .sessions()
                .load_sessions_for_project_page(project, limit, Some(pinned_id.clone()))
                .await
                .expect("pinned archive page");

            // Assert
            assert_eq!(rows.len(), visible_count);
            assert!(rows.iter().any(|row| row.id == pinned_id));
            assert_eq!(has_more, has_omitted_archive);
        }
    }
}

#[tokio::test]
async fn session_archive_pages_bound_rows_and_preserve_project_and_detail_scope() {
    // Arrange
    let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 123))
        .await
        .expect("database");
    let project = database
        .projects()
        .upsert_project("archive-project", None)
        .await
        .expect("project");
    let other_project = database
        .projects()
        .upsert_project("other-project", None)
        .await
        .expect("other project");
    for index in 0..23 {
        let id = format!("archive-{index:02}");
        database
            .sessions()
            .insert_session(
                &id,
                "gpt-5.6-sol",
                "main",
                if index % 2 == 0 { "Done" } else { "Canceled" },
                project,
            )
            .await
            .expect("archive");
        database
            .sessions()
            .update_session_updated_at(&id, 100 - index)
            .await
            .expect("timestamp");
    }
    for (id, status) in [
        ("active", "Review"),
        ("queue", "Queued"),
        ("merged", "Merged"),
    ] {
        database
            .sessions()
            .insert_session(id, "gpt-5.6-sol", "main", status, project)
            .await
            .expect("active row");
    }
    database
        .sessions()
        .insert_session("other", "gpt-5.6-sol", "main", "Done", other_project)
        .await
        .expect("other row");

    for (limit, expected_count, expected_more) in [
        (0, 3, true),
        (10, 13, true),
        (20, 23, true),
        (23, 26, false),
        (30, 26, false),
        (usize::MAX, 26, false),
    ] {
        // Act
        let (rows, has_more) = database
            .sessions()
            .load_sessions_for_project_page(project, limit, None)
            .await
            .expect("page");

        // Assert
        assert_eq!(rows.len(), expected_count);
        assert_eq!(has_more, expected_more);
        let archived = rows
            .iter()
            .filter(|row| matches!(row.status.as_str(), "Done" | "Canceled"))
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>();
        let expected = (0..limit.min(23))
            .map(|index| format!("archive-{index:02}"))
            .collect::<Vec<_>>();
        assert_eq!(archived, expected);
        assert!(rows.iter().all(|row| row.project_id == Some(project)));
    }

    // Act
    let (pinned, has_more) = database
        .sessions()
        .load_sessions_for_project_page(project, 10, Some("archive-22".into()))
        .await
        .expect("pinned page");
    let (other, other_has_more) = database
        .sessions()
        .load_sessions_for_project_page(other_project, 10, Some("archive-22".into()))
        .await
        .expect("other page");
    let (empty, empty_has_more) = database
        .sessions()
        .load_sessions_for_project_page(999, 10, None)
        .await
        .expect("empty page");

    // Assert
    assert_eq!(pinned.len(), 14);
    assert!(pinned.iter().any(|row| row.id == "archive-22"));
    assert!(!pinned.iter().any(|row| row.id == "archive-10"));
    assert!(has_more);
    assert_eq!(other.len(), 1);
    assert!(!other_has_more);
    assert!(empty.is_empty());
    assert!(!empty_has_more);
}

#[tokio::test]
async fn archived_session_count_includes_all_pages_and_tracks_project_status_changes() {
    // Arrange
    let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 123))
        .await
        .expect("database");
    let project = database
        .projects()
        .upsert_project("archive-project", None)
        .await
        .expect("project");
    let other_project = database
        .projects()
        .upsert_project("other-project", None)
        .await
        .expect("other project");
    for status in [
        "Draft", "Review", "Queued", "Merging", "Merged", "Done", "Canceled",
    ] {
        database
            .sessions()
            .insert_session(status, "gpt-5.6-sol", "main", status, project)
            .await
            .expect("row");
    }
    database
        .sessions()
        .insert_session("other", "gpt-5.6-sol", "main", "Done", other_project)
        .await
        .expect("other archive");

    // Act & Assert
    for (project_id, expected_total) in [(project, 2), (other_project, 1), (999, 0)] {
        let (_, has_more) = database
            .sessions()
            .load_sessions_for_project_page(project_id, 1, None)
            .await
            .expect("page");
        let total = database
            .sessions()
            .load_archived_session_count(project_id)
            .await
            .expect("total");
        assert_eq!(total, expected_total);
        assert_eq!(has_more, expected_total > 1);
    }

    // Act: an active session is archived and an existing archive is deleted.
    database
        .sessions()
        .update_session_status_with_timing_at("Review", "Canceled", 123)
        .await
        .expect("archive session");
    let after_archive = database
        .sessions()
        .load_archived_session_count(project)
        .await
        .expect("updated total");
    database
        .sessions()
        .delete_session("Done")
        .await
        .expect("delete archive");
    let after_delete = database
        .sessions()
        .load_archived_session_count(project)
        .await
        .expect("updated total");

    // Assert
    assert_eq!(after_archive, 3);
    assert_eq!(after_delete, 2);
}

#[tokio::test]
async fn archived_session_count_uses_covering_project_status_index() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");

    // Act
    let plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
        "EXPLAIN QUERY PLAN SELECT COUNT(*) FROM session WHERE project_id = ? AND status IN \
         ('Done', 'Canceled')",
    )
    .bind(1_i64)
    .fetch_all(database.pool())
    .await
    .expect("archive count query plan");

    // Assert
    assert!(plan.iter().any(|(_, _, _, detail)| {
        detail.contains("SEARCH session USING COVERING INDEX idx_session_project_status")
    }));
    assert!(
        plan.iter()
            .all(|(_, _, _, detail)| !detail.contains("SCAN session"))
    );
}
