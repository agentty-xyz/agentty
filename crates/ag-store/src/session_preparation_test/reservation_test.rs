use super::support::reservation;
use crate::session_preparation::SessionPreparationState;
use crate::{AppRepositories, ForkSessionSnapshot};

#[tokio::test]
async fn reservation_retains_first_prompt_across_failure_recovery() {
    // Arrange
    let (repositories, _) = AppRepositories::in_memory_with_pool().await.expect("db");
    let project_id = repositories
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    let sessions = repositories.sessions();
    sessions
        .reserve_session(reservation("session", project_id))
        .await
        .expect("reserve");

    // Act
    assert!(
        sessions
            .save_preparation_prompt("session", "first prompt")
            .await
            .expect("save")
    );
    assert!(
        !sessions
            .save_preparation_prompt("session", "replacement")
            .await
            .expect("duplicate")
    );
    sessions
        .insert_session_preparation("session", "other-ref")
        .await
        .expect("idempotent insert");
    sessions
        .recover_session_preparations()
        .await
        .expect("recover");
    let recovered = sessions
        .load_session_preparation("session")
        .await
        .expect("load")
        .expect("row");
    let project_rows = sessions
        .load_session_preparations(project_id)
        .await
        .expect("project rows");
    sessions
        .update_session_preparation("session", SessionPreparationState::Preparing, None)
        .await
        .expect("retry");
    sessions
        .update_session_preparation("session", SessionPreparationState::Ready, None)
        .await
        .expect("ready");
    sessions
        .recover_session_preparations()
        .await
        .expect("recover handoff");
    let interrupted_handoff = sessions
        .load_session_preparation("session")
        .await
        .expect("load")
        .expect("row");
    sessions
        .clear_preparation_prompt("session")
        .await
        .expect("acknowledge");

    // Assert
    assert_eq!(recovered.state, SessionPreparationState::Failed);
    assert_eq!(recovered.start_ref, "main");
    assert_eq!(recovered.prompt.as_deref(), Some("first prompt"));
    assert!(recovered.error.expect("reason").contains("interrupted"));
    assert_eq!(project_rows.len(), 1);
    assert_eq!(interrupted_handoff.state, SessionPreparationState::Failed);
    assert!(
        sessions
            .load_session_preparation("missing")
            .await
            .expect("missing")
            .is_none()
    );
    assert!(
        sessions
            .load_session_preparations(project_id + 1)
            .await
            .expect("other project")
            .is_empty()
    );
    assert_eq!(
        sessions
            .load_session("session")
            .await
            .expect("session")
            .expect("row")
            .status,
        "Draft"
    );
}

#[tokio::test]
async fn reservation_rolls_back_when_preparation_cannot_be_persisted() {
    // Arrange
    let (repositories, pool) = AppRepositories::in_memory_with_pool().await.expect("db");
    let project_id = repositories
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    sqlx::query(
        "CREATE TRIGGER reject_preparation BEFORE INSERT ON session_preparation BEGIN SELECT \
         RAISE(ABORT, 'rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("trigger");

    // Act
    let result = repositories
        .sessions()
        .reserve_session(reservation("rejected", project_id))
        .await;
    let row = repositories
        .sessions()
        .load_session("rejected")
        .await
        .expect("load");

    // Assert
    assert!(result.is_err());
    assert!(row.is_none());
}

#[tokio::test]
async fn lazy_and_fork_preparation_preserve_identity_and_handoff_evidence() {
    // Arrange
    let (repositories, _) = AppRepositories::in_memory_with_pool().await.expect("db");
    let project_id = repositories
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    let sessions = repositories.sessions();
    sessions
        .insert_session_with_agent(reservation("source", project_id))
        .await
        .expect("source");

    // Act
    sessions
        .insert_session_preparation("source", "parent-tip")
        .await
        .expect("lazy preparation");
    sessions
        .reserve_fork_session_snapshot(
            ForkSessionSnapshot {
                new_session_id: "fork",
                source_session_id: "source",
                status: "Review",
            },
            "frozen-commit",
        )
        .await
        .expect("fork");
    let before = sessions
        .preparation_prompt_operation_status("fork")
        .await
        .expect("unsubmitted");
    repositories
        .operations()
        .insert_session_operation("workspace:fork", "fork", "reply")
        .await
        .expect("handoff");
    let queued = sessions
        .preparation_prompt_operation_status("fork")
        .await
        .expect("queued");
    repositories
        .operations()
        .mark_session_operation_done("workspace:fork")
        .await
        .expect("done");
    let completed = sessions
        .preparation_prompt_operation_status("fork")
        .await
        .expect("completed");
    let fork = sessions
        .load_session_preparation("fork")
        .await
        .expect("load")
        .expect("fork preparation");

    // Assert
    assert!(before.is_none());
    assert_eq!(queued.as_deref(), Some("queued"));
    assert_eq!(completed.as_deref(), Some("done"));
    assert_eq!(fork.start_ref, "frozen-commit");
    assert_eq!(fork.state, SessionPreparationState::Preparing);
}
