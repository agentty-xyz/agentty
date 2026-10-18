//! Task contracts through the public, migrated SQLite adapter.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use ag_session::{
    CreateTaskRequest, DurableMessageVisibility, SubmitTaskRequest, Task, TaskAttempt,
    TaskAttemptStatus,
};
use ag_store::{Database, DbError};

fn request(attempt_id: &str) -> SubmitTaskRequest {
    SubmitTaskRequest {
        attempt_id: attempt_id.to_string(),
        message: "Implement the outcome".to_string(),
        objective: "A working feature".to_string(),
        task_id: "task".to_string(),
        visibility: DurableMessageVisibility::Visible,
    }
}

async fn database() -> Result<Database, DbError> {
    let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 123)).await?;
    let project = database.projects().upsert_project("project", None).await?;
    for session in ["session", "other"] {
        database
            .sessions()
            .insert_session(session, "gpt-5.6-sol", "main", "Review", project)
            .await?;
    }

    Ok(database)
}

#[tokio::test]
async fn planned_tasks_preserve_identity_and_creation_time_when_attempted_or_completed() {
    // Arrange
    let now = Arc::new(AtomicI64::new(123));
    let database = Database::open_in_memory_with_timestamp_source({
        let now = Arc::clone(&now);
        Arc::new(move || now.load(Ordering::Relaxed))
    })
    .await
    .expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "gpt-5.6-sol", "main", "Review", project)
        .await
        .expect("session");
    let tasks = database.tasks();
    let attempt = request("first");
    let planned = CreateTaskRequest {
        objective: attempt.objective.clone(),
        task_id: attempt.task_id.clone(),
    };

    // Act
    tasks.create_task("session", &planned).await.expect("plan");
    let original = tasks.load_task("task").await.expect("load").expect("task");
    now.store(456, Ordering::Relaxed);
    tasks
        .create_task("session", &planned)
        .await
        .expect("redelivery");

    // Assert
    assert_eq!(
        tasks.load_task("task").await.expect("unchanged"),
        Some(original.clone())
    );
    assert_eq!(original.created_at, 123);
    assert_eq!(original.completed_at, None);
    assert_eq!(original.attempts, Vec::<TaskAttempt>::new());
    let operations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_operation")
        .fetch_one(database.pool())
        .await
        .expect("operations");
    assert_eq!(operations, 0);

    // Act
    assert!(
        tasks
            .claim_attempt("session", &attempt, "reply")
            .await
            .expect("submit planned")
    );
    database
        .operations()
        .mark_session_operation_done(&attempt.operation_id())
        .await
        .expect("settle");
    tasks.complete_task("task").await.expect("accept");
    let completed = tasks.load_task("task").await.expect("load").expect("task");
    now.store(789, Ordering::Relaxed);
    tasks
        .create_task("session", &planned)
        .await
        .expect("redelivery after completion");

    // Assert
    assert_eq!(completed.created_at, 123);
    assert_eq!(completed.completed_at, Some(456));
    assert_eq!(completed.attempts[0].queued_at, 456);
    assert_eq!(
        tasks.list_tasks("session").await.expect("history"),
        vec![completed]
    );
}

#[tokio::test]
async fn task_listing_is_session_scoped_and_includes_unattempted_completed_outcomes() {
    // Arrange
    let database = database().await.expect("database");
    let tasks = database.tasks();
    for (session, task_id) in [
        ("session", "z-first"),
        ("other", "unrelated"),
        ("session", "a-second"),
    ] {
        tasks
            .create_task(
                session,
                &CreateTaskRequest {
                    objective: "Planned outcome".to_string(),
                    task_id: task_id.to_string(),
                },
            )
            .await
            .expect("plan");
    }

    // Act
    tasks
        .complete_task("z-first")
        .await
        .expect("accept without execution");
    let listed = tasks.list_tasks("session").await.expect("list");

    // Assert
    assert_eq!(
        listed
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["z-first", "a-second"]
    );
    assert!(listed.iter().all(|task| task.attempts.is_empty()));
    assert_eq!(listed[0].completed_at, Some(123));
    assert_eq!(listed[1].completed_at, None);
    assert_eq!(
        tasks.list_tasks("missing").await.expect("empty"),
        Vec::<Task>::new()
    );

    // Act
    database
        .sessions()
        .delete_session("session")
        .await
        .expect("delete owner");

    // Assert
    assert_eq!(
        tasks.list_tasks("session").await.expect("deleted"),
        Vec::<Task>::new()
    );
    assert_eq!(
        tasks.list_tasks("other").await.expect("other owner").len(),
        1
    );
}

#[tokio::test]
async fn task_creation_rejects_empty_inputs_missing_sessions_and_identity_conflicts() {
    // Arrange
    let database = database().await.expect("database");
    let tasks = database.tasks();
    let planned = CreateTaskRequest {
        objective: "Outcome".to_string(),
        task_id: "planned".to_string(),
    };

    // Act / Assert
    for (task_id, objective) in [(" \t", "Outcome"), ("planned", "\n")] {
        assert!(
            tasks
                .create_task(
                    "session",
                    &CreateTaskRequest {
                        objective: objective.to_string(),
                        task_id: task_id.to_string(),
                    }
                )
                .await
                .is_err()
        );
    }
    assert!(tasks.create_task("missing", &planned).await.is_err());
    assert_eq!(
        tasks
            .list_tasks("session")
            .await
            .expect("no partial writes"),
        Vec::<Task>::new()
    );
    tasks.create_task("session", &planned).await.expect("plan");
    for (session, objective) in [("other", "Outcome"), ("session", "Changed")] {
        assert!(
            tasks
                .create_task(
                    session,
                    &CreateTaskRequest {
                        objective: objective.to_string(),
                        ..planned.clone()
                    }
                )
                .await
                .is_err()
        );
    }
    let original = tasks
        .load_task("planned")
        .await
        .expect("load")
        .expect("task");
    assert_eq!(original.objective, "Outcome");
    assert_eq!(original.session_id.as_str(), "session");
}

#[tokio::test]
async fn task_attempts_keep_history_across_retries_cancellation_and_restart() {
    // Arrange
    let database = database().await.expect("database");
    let tasks = database.tasks();
    let operations = database.operations();
    assert_eq!(tasks.load_task("missing").await.expect("missing"), None);
    assert!(tasks.complete_task("missing").await.is_err());

    for (index, expected) in [
        TaskAttemptStatus::Failed,
        TaskAttemptStatus::Canceled,
        TaskAttemptStatus::Succeeded,
    ]
    .into_iter()
    .enumerate()
    {
        let request = request(&format!("attempt-{index}"));

        // Act
        assert!(
            tasks
                .claim_attempt("session", &request, "reply")
                .await
                .expect("claim")
        );
        assert!(
            tasks
                .contains_attempt("session", &request)
                .await
                .expect("contains")
        );
        assert!(
            !tasks
                .claim_attempt("session", &request, "reply")
                .await
                .expect("duplicate")
        );
        assert!(tasks.complete_task("task").await.is_err());
        let queued = tasks.load_task("task").await.expect("load").expect("task");
        assert_eq!(queued.attempts[index].status, TaskAttemptStatus::Queued);
        operations
            .mark_session_operation_running(&request.operation_id())
            .await
            .expect("start");
        let running = tasks.load_task("task").await.expect("load").expect("task");
        assert_eq!(running.attempts[index].status, TaskAttemptStatus::Running);
        match expected {
            TaskAttemptStatus::Failed => operations
                .fail_unfinished_session_operations("Interrupted by app restart")
                .await
                .expect("recover"),
            TaskAttemptStatus::Canceled => {
                operations
                    .request_cancel_for_session_operations("session")
                    .await
                    .expect("cancel");
                operations
                    .mark_session_operation_done(&request.operation_id())
                    .await
                    .expect("settle canceled");
            }
            _ => operations
                .mark_session_operation_done(&request.operation_id())
                .await
                .expect("settle"),
        }

        // Assert
        let task = tasks.load_task("task").await.expect("load").expect("task");
        assert_eq!(task.created_at, 123);
        assert_eq!(task.completed_at, None);
        assert_eq!(task.session_id.as_str(), "session");
        assert_eq!(task.objective, request.objective);
        assert_eq!(task.attempts.len(), index + 1);
        let attempt = &task.attempts[index];
        assert_eq!(attempt.status, expected);
        assert_eq!(attempt.queued_at, 123);
        assert_eq!(attempt.started_at, Some(123));
        assert_eq!(attempt.finished_at, Some(123));
        assert_eq!(attempt.message, request.message);
        assert_eq!(attempt.visibility, DurableMessageVisibility::Visible);
        assert!(
            !tasks
                .claim_attempt("session", &request, "reply")
                .await
                .expect("terminal duplicate")
        );
        let reopened = operations
            .claim_session_operation(&request.operation_id(), "session", "reply")
            .await;
        if expected == TaskAttemptStatus::Succeeded {
            assert!(!reopened.expect("completed operation stays closed"));
        } else {
            assert!(reopened.is_err());
        }
        assert_eq!(
            tasks.load_task("task").await.expect("unchanged"),
            Some(task)
        );
    }
    let task = tasks.load_task("task").await.expect("load").expect("task");
    assert_eq!(
        task.attempts[0].last_error.as_deref(),
        Some("Interrupted by app restart")
    );
}

#[tokio::test]
async fn task_completion_is_explicit_and_preserves_delivery_receipts() {
    // Arrange
    let database = database().await.expect("database");
    let tasks = database.tasks();
    let request = request("attempt");
    tasks
        .claim_attempt("session", &request, "reply")
        .await
        .expect("claim");
    database
        .operations()
        .mark_session_operation_done(&request.operation_id())
        .await
        .expect("settle");

    // Act
    tasks
        .complete_task("task")
        .await
        .expect("explicit completion");
    tasks
        .complete_task("task")
        .await
        .expect("idempotent completion");
    let task = tasks.load_task("task").await.expect("load").expect("task");

    // Assert
    assert_eq!(task.completed_at, Some(123));
    assert!(
        !tasks
            .claim_attempt("session", &request, "reply")
            .await
            .expect("completed duplicate")
    );
    let retry = SubmitTaskRequest {
        attempt_id: "retry".to_string(),
        ..request
    };
    assert!(
        tasks
            .claim_attempt("session", &retry, "reply")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn task_admission_validates_inputs_and_rolls_back_partial_records() {
    // Arrange
    let database = database().await.expect("database");
    let tasks = database.tasks();
    let original = request("attempt");

    // Act / Assert
    for field in 0..4 {
        let mut invalid = original.clone();
        match field {
            0 => invalid.task_id = " ".to_string(),
            1 => invalid.attempt_id.clear(),
            2 => invalid.objective.clear(),
            _ => invalid.message.clear(),
        }
        assert!(
            tasks
                .claim_attempt("session", &invalid, "reply")
                .await
                .is_err()
        );
    }
    assert!(
        tasks
            .claim_attempt("missing", &original, "reply")
            .await
            .is_err()
    );
    assert_eq!(
        tasks.load_task("task").await.expect("no partial task"),
        None
    );
    database
        .operations()
        .insert_session_operation(&original.operation_id(), "session", "reply")
        .await
        .expect("collision");
    assert!(
        tasks
            .claim_attempt("session", &original, "reply")
            .await
            .is_err()
    );
    assert_eq!(tasks.load_task("task").await.expect("rolled back"), None);
}

#[tokio::test]
async fn task_admission_rejects_conflicting_and_concurrent_attempts() {
    // Arrange
    let database = database().await.expect("database");
    let tasks = database.tasks();
    let original = request("accepted");
    assert!(
        tasks
            .claim_attempt("session", &original, "reply")
            .await
            .expect("claim")
    );

    // Act / Assert
    for field in 0..5 {
        let mut conflict = original.clone();
        let session = if field == 0 { "other" } else { "session" };
        match field {
            1 => conflict.task_id = "other-task".to_string(),
            2 => conflict.objective = "different".to_string(),
            3 => conflict.message = "different".to_string(),
            4 => conflict.visibility = DurableMessageVisibility::Hidden,
            _ => {}
        }
        assert!(tasks.contains_attempt(session, &conflict).await.is_err());
        assert!(
            tasks
                .claim_attempt(session, &conflict, "reply")
                .await
                .is_err()
        );
    }
    assert_eq!(tasks.load_task("other-task").await.expect("rollback"), None);
    assert!(
        tasks
            .claim_attempt("session", &request("concurrent"), "reply")
            .await
            .is_err()
    );
    database
        .operations()
        .mark_session_operation_failed(&original.operation_id(), "failed")
        .await
        .expect("failure");
    assert!(
        tasks
            .claim_attempt("other", &request("wrong-session"), "reply")
            .await
            .is_err()
    );
    let mut conflict = request("wrong-objective");
    conflict.objective = "different".to_string();
    assert!(
        tasks
            .claim_attempt("session", &conflict, "reply")
            .await
            .is_err()
    );
    let mut hidden = request("hidden");
    hidden.visibility = DurableMessageVisibility::Hidden;
    assert!(
        tasks
            .claim_attempt("session", &hidden, "reply")
            .await
            .expect("hidden")
    );
    assert_eq!(
        tasks
            .load_task("task")
            .await
            .expect("load")
            .expect("task")
            .attempts[1]
            .visibility,
        DurableMessageVisibility::Hidden
    );
    sqlx::query("UPDATE session_operation SET status = 'invalid' WHERE id = ?")
        .bind(hidden.operation_id())
        .execute(database.pool())
        .await
        .expect("corrupt status");
    assert!(tasks.load_task("task").await.is_err());
}
