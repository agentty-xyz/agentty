use ag_session::{OrchestrationStatus, OrchestrationTaskStatus};

use super::support::{controller_fixture, insert_orchestration_child, planned_task};
use crate::{AppRepositories, DbError};

#[tokio::test]
async fn rollup_recovery_failures_report_semantic_operation_context() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    sqlx::query("DROP TABLE session_orchestration")
        .execute(&pool)
        .await
        .expect("failed to drop orchestration table");

    // Act
    let claim_error = database
        .orchestrations()
        .claim_orchestration_rollup(1)
        .await
        .expect_err("claim should fail");
    let completion_error = database
        .orchestrations()
        .complete_orchestration_rollup(1)
        .await
        .expect_err("completion should fail");

    // Assert
    assert!(matches!(
        claim_error,
        DbError::QueryContext {
            operation: "claim orchestration rollup",
            ..
        }
    ));
    assert!(matches!(
        completion_error,
        DbError::QueryContext {
            operation: "complete orchestration rollup",
            ..
        }
    ));
}

#[tokio::test]
/// Loads running, submitting, and canceling orchestrations while excluding
/// terminal rows.
async fn test_active_orchestration_load_includes_recoverable_states() {
    // Arrange
    let database = controller_fixture().await;
    let running_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert running orchestration");
    let submitting_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert submitting orchestration");
    let canceling_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Canceling.to_string(), 2)
        .await
        .expect("failed to insert canceling orchestration");
    let settled_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert settled orchestration");

    // Act
    database
        .orchestrations()
        .update_orchestration_status(settled_id, &OrchestrationStatus::Done.to_string())
        .await
        .expect("failed to settle orchestration");
    let first_claim = database
        .orchestrations()
        .claim_orchestration_rollup(submitting_id)
        .await
        .expect("failed to claim roll-up");
    let duplicate_claim = database
        .orchestrations()
        .claim_orchestration_rollup(submitting_id)
        .await
        .expect("failed to repeat roll-up claim");
    let active = database
        .orchestrations()
        .load_active_orchestrations()
        .await
        .expect("failed to load active orchestrations");

    // Assert
    assert!(first_claim);
    assert!(!duplicate_claim);
    assert_eq!(
        active.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![running_id, submitting_id, canceling_id]
    );
    assert_eq!(active[1].status, OrchestrationStatus::Verifying.to_string());
    assert_eq!(active[1].verification_generation, 1);
    assert_eq!(active[2].status, OrchestrationStatus::Canceling.to_string());
}

#[tokio::test]
/// Uses the cancellation status as a durable barrier against both a late
/// task claim and a late child link.
async fn test_cancellation_barrier_blocks_fan_out_claims_and_links() {
    // Arrange
    let database = controller_fixture().await;
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to reload project");
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert orchestration");
    let late_task_id = database
        .orchestrations()
        .upsert_orchestration_task(planned_task(orchestration_id, "late"))
        .await
        .expect("failed to insert late task");
    let claimed_task_id = database
        .orchestrations()
        .upsert_orchestration_task(planned_task(orchestration_id, "claimed"))
        .await
        .expect("failed to insert claimed task");
    let initial_claim = database
        .orchestrations()
        .claim_orchestration_task(claimed_task_id)
        .await
        .expect("failed to claim task before cancellation");
    insert_orchestration_child(&database, project_id, "child-claimed", claimed_task_id).await;

    // Act
    let cancellation_started = database
        .orchestrations()
        .begin_orchestration_cancellation(orchestration_id)
        .await
        .expect("failed to begin cancellation");
    let cancellation_retried = database
        .orchestrations()
        .begin_orchestration_cancellation(orchestration_id)
        .await
        .expect("failed to retry cancellation");
    let late_claim = database
        .orchestrations()
        .claim_orchestration_task(late_task_id)
        .await
        .expect("failed to inspect late claim");
    let late_link = database
        .orchestrations()
        .link_orchestration_task_child(claimed_task_id, "child-claimed")
        .await
        .expect("failed to inspect late link");
    let rollup_completion = database
        .orchestrations()
        .complete_orchestration_rollup(orchestration_id)
        .await
        .expect("failed to inspect late roll-up completion");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load orchestration")
        .expect("orchestration should exist");
    // Assert
    assert!(initial_claim);
    assert!(cancellation_started);
    assert!(cancellation_retried);
    assert!(!late_claim);
    assert!(!late_link);
    assert!(!rollup_completion);
    assert_eq!(
        orchestration.status,
        OrchestrationStatus::Canceling.to_string()
    );
}

#[tokio::test]
/// Observes the durable worker operation until successful completion and
/// only then settles its submitting orchestration.
async fn test_rollup_operation_status_controls_orchestration_completion() {
    // Arrange
    let database = controller_fixture().await;
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert orchestration");
    let claimed = database
        .orchestrations()
        .claim_orchestration_rollup(orchestration_id)
        .await
        .expect("failed to claim roll-up");
    assert!(claimed);
    let operation_id = format!("orchestration-rollup-{orchestration_id}-1");

    // Act
    let missing_status = database
        .orchestrations()
        .load_rollup_operation_status(&operation_id)
        .await
        .expect("failed to load missing operation");
    database
        .operations()
        .claim_session_operation(&operation_id, "controller", "reply")
        .await
        .expect("failed to claim operation");
    let queued_status = database
        .orchestrations()
        .load_rollup_operation_status(&operation_id)
        .await
        .expect("failed to load queued operation");
    database
        .operations()
        .mark_session_operation_running(&operation_id)
        .await
        .expect("failed to run operation");
    let running_status = database
        .orchestrations()
        .load_rollup_operation_status(&operation_id)
        .await
        .expect("failed to load running operation");
    database
        .operations()
        .mark_session_operation_failed(&operation_id, "turn failed")
        .await
        .expect("failed to fail operation");
    let failed_status = database
        .orchestrations()
        .load_rollup_operation_status(&operation_id)
        .await
        .expect("failed to load failed operation");
    database
        .operations()
        .claim_session_operation(&operation_id, "controller", "reply")
        .await
        .expect("failed to reclaim operation");
    database
        .operations()
        .mark_session_operation_done(&operation_id)
        .await
        .expect("failed to complete operation");
    let done_status = database
        .orchestrations()
        .load_rollup_operation_status(&operation_id)
        .await
        .expect("failed to load completed operation");
    let completed = database
        .orchestrations()
        .complete_orchestration_rollup(orchestration_id)
        .await
        .expect("failed to complete orchestration");
    let duplicate_completion = database
        .orchestrations()
        .complete_orchestration_rollup(orchestration_id)
        .await
        .expect("failed to inspect duplicate completion");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load orchestration")
        .expect("orchestration should exist");

    // Assert
    assert_eq!(missing_status, None);
    assert_eq!(queued_status.as_deref(), Some("queued"));
    assert_eq!(running_status.as_deref(), Some("running"));
    assert_eq!(failed_status.as_deref(), Some("failed"));
    assert_eq!(done_status.as_deref(), Some("done"));
    assert!(completed);
    assert!(!duplicate_completion);
    assert_eq!(
        orchestration.status,
        OrchestrationStatus::AwaitingIntegration.to_string()
    );
}

#[tokio::test]
async fn infrastructure_retries_are_bounded_and_completion_is_idempotent() {
    // Arrange
    let database = controller_fixture().await;
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert orchestration");
    let task_id = database
        .orchestrations()
        .upsert_orchestration_task(planned_task(orchestration_id, "alpha"))
        .await
        .expect("failed to insert task");

    // Act
    let first_status = database
        .orchestrations()
        .record_orchestration_spawn_failure(task_id, "provider unavailable", 2)
        .await
        .expect("failed to record first retry");
    let second_status = database
        .orchestrations()
        .record_orchestration_spawn_failure(task_id, "provider unavailable", 2)
        .await
        .expect("failed to record second retry");
    let final_status = database
        .orchestrations()
        .record_orchestration_spawn_failure(task_id, "provider unavailable", 2)
        .await
        .expect("failed to exhaust retries");
    database
        .orchestrations()
        .update_orchestration_status(
            orchestration_id,
            &OrchestrationStatus::Integrating.to_string(),
        )
        .await
        .expect("failed to begin integration completion");
    let completed = database
        .orchestrations()
        .complete_orchestration_campaign(orchestration_id)
        .await
        .expect("failed to complete campaign");
    let duplicate_completion = database
        .orchestrations()
        .complete_orchestration_campaign(orchestration_id)
        .await
        .expect("failed to inspect duplicate completion");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load completed campaign")
        .expect("completed campaign should exist");
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration_id)
        .await
        .expect("failed to load exhausted task")
        .remove(0);
    let controller = database
        .sessions()
        .load_session("controller")
        .await
        .expect("failed to load completed controller")
        .expect("controller should exist");

    // Assert
    assert_eq!(first_status, OrchestrationTaskStatus::Planned.to_string());
    assert_eq!(second_status, OrchestrationTaskStatus::Planned.to_string());
    assert_eq!(final_status, OrchestrationTaskStatus::Failed.to_string());
    assert!(completed);
    assert!(!duplicate_completion);
    assert_eq!(task.infrastructure_retry_count, 3);
    assert_eq!(task.status, OrchestrationTaskStatus::Failed.to_string());
    assert_eq!(orchestration.status, OrchestrationStatus::Done.to_string());
    assert_eq!(controller.status, "Done");
}
