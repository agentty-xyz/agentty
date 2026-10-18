//! Task contracts through the public, migrated SQLite adapter.

use std::sync::Arc;

use ag_session::{DurableMessageVisibility, SubmitTaskRequest, TaskAttemptStatus};
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
