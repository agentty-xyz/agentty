use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use ag_worker::{RunInfo, RunState};

use crate::Database;

#[tokio::test]
async fn utility_lifecycle_is_durable_and_terminal_states_are_not_resurrected() {
    // Arrange
    let now = Arc::new(AtomicI64::new(1));
    let timestamp = now.clone();
    let db = Database::open_in_memory_with_timestamp_source(Arc::new(move || {
        timestamp.load(Ordering::SeqCst)
    }))
    .await
    .expect("database");
    let repository = db.runs();
    let run = RunInfo {
        folder: "repository".into(),
        id: "run".into(),
        parent_id: Some("workflow".into()),
        project_id: None,
        purpose: "summary".into(),
        session_id: None,
    };
    // Act
    repository.create(&run).await.expect("queued");
    repository
        .transition("run", RunState::Queued, None)
        .await
        .expect("queued remains queued");
    now.store(2, Ordering::SeqCst);
    repository
        .transition("run", RunState::Running, None)
        .await
        .expect("running");
    now.store(3, Ordering::SeqCst);
    repository.heartbeat("run").await.expect("heartbeat");
    let heartbeat: (i64,) = sqlx::query_as("SELECT heartbeat_at FROM agent_run")
        .fetch_one(db.pool())
        .await
        .expect("heartbeat row");
    now.store(4, Ordering::SeqCst);
    repository
        .transition("run", RunState::Completed, None)
        .await
        .expect("completed");
    now.store(5, Ordering::SeqCst);
    repository
        .transition("run", RunState::Running, None)
        .await
        .expect("no resurrection");
    repository
        .heartbeat("run")
        .await
        .expect("no stale heartbeat");
    repository.recover().await.expect("preserve completion");
    // Assert
    assert_eq!(heartbeat.0, 3);
    let row: (String, String, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT status, parent_id, queued_at, started_at, heartbeat_at, finished_at FROM agent_run",
    )
    .fetch_one(db.pool())
    .await
    .expect("row");
    assert_eq!(row, ("completed".into(), "workflow".into(), 1, 2, 4, 4));
    assert!(repository.create(&run).await.is_err());
}

#[tokio::test]
async fn recovery_fails_abandoned_runs_and_preserves_cancellation() {
    // Arrange
    let db = Database::open_in_memory().await.expect("database");
    let repository = db.runs();
    for id in ["queued", "running", "canceled"] {
        repository
            .create(&RunInfo {
                folder: "project".into(),
                id: id.into(),
                parent_id: None,
                project_id: None,
                purpose: "review".into(),
                session_id: None,
            })
            .await
            .expect("create");
    }
    repository
        .transition("running", RunState::Running, None)
        .await
        .expect("running");
    repository
        .transition("canceled", RunState::Canceled, Some("stopped"))
        .await
        .expect("canceled");
    // Act
    repository.recover().await.expect("recover");
    // Assert
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT status, last_error FROM agent_run ORDER BY id")
            .fetch_all(db.pool())
            .await
            .expect("records");
    assert_eq!(rows[0], ("canceled".into(), "stopped".into()));
    assert!(
        rows[1..]
            .iter()
            .all(|(state, error)| state == "failed" && error.contains("restart"))
    );
    db.pool().close().await;
    assert!(repository.heartbeat("queued").await.is_err());
    assert!(
        repository
            .transition("queued", RunState::Failed, None)
            .await
            .is_err()
    );
    assert!(repository.recover().await.is_err());
    assert!(repository.close_session("closed").await.is_err());
}

#[tokio::test]
async fn closed_session_admission_survives_deletion_and_keeps_other_work_open() {
    // Arrange
    let db = Database::open_in_memory().await.expect("database");
    let project = db
        .projects()
        .upsert_project("repository", None)
        .await
        .expect("project");
    db.sessions()
        .insert_session("closed", "model", "main", "Review", project)
        .await
        .expect("session");
    let repository = db.runs();
    let mut run = RunInfo {
        folder: "repository".into(),
        id: "late".into(),
        parent_id: None,
        project_id: Some(project),
        purpose: "title".into(),
        session_id: Some("closed".into()),
    };
    // Act
    repository.close_session("closed").await.expect("close");
    repository
        .close_session("closed")
        .await
        .expect("idempotent close");
    // Assert
    assert!(
        repository
            .create(&run)
            .await
            .expect_err("closed session")
            .to_string()
            .contains("closed")
    );
    db.sessions()
        .delete_session("closed")
        .await
        .expect("delete");
    assert!(
        repository
            .create(&run)
            .await
            .expect_err("deleted session")
            .to_string()
            .contains("closed")
    );
    run.session_id = None;
    repository
        .create(&run)
        .await
        .expect("project work remains open");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_run_closed_session")
        .fetch_one(db.pool())
        .await
        .expect("one tombstone");
    assert_eq!(count, 1);
}
