use ag_agent::{AgentKind, ReasoningLevel, SpeedMode};
use ag_session::{
    OrchestrationStatus, OrchestrationTaskKind, OrchestrationTaskStatus, SessionMessageKind,
};

use super::support::{
    controller_fixture, controller_fixture_with_pool, insert_orchestration_child, planned_task,
};
use crate::orchestration::{PersistedOrchestrationTask, SessionOrchestrationMetadataRow};
use crate::{AppRepositories, DbError, PersistedSessionCreation};

#[tokio::test]
async fn hydration_rejects_unknown_orchestration_status() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/invalid-orchestration", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("controller", "gpt-5.6-sol", "main", "Draft", project_id)
        .await
        .expect("failed to insert controller session");
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration("controller", "Running", 1)
        .await
        .expect("failed to insert orchestration");
    sqlx::query("UPDATE session_orchestration SET status = 'Unknown' WHERE id = ?")
        .bind(orchestration_id)
        .execute(&pool)
        .await
        .expect("failed to corrupt orchestration status");

    // Act
    let error = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect_err("invalid status should fail hydration");

    // Assert
    assert!(matches!(
        error,
        DbError::InvalidStatus {
            entity: "orchestration",
            value,
        } if value == "Unknown"
    ));
}

#[tokio::test]
/// Persists one plan before any child exists and loads it back for the
/// owning controller session.
async fn test_planned_orchestration_round_trips_before_any_child_exists() {
    // Arrange
    let database = controller_fixture().await;

    // Act
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration(
            "controller",
            &OrchestrationStatus::AwaitingApproval.to_string(),
            3,
        )
        .await
        .expect("failed to insert orchestration");
    database
        .orchestrations()
        .upsert_orchestration_task(planned_task(orchestration_id, "alpha"))
        .await
        .expect("failed to insert task");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load orchestration")
        .expect("orchestration should exist");
    let tasks = database
        .orchestrations()
        .load_orchestration_tasks(orchestration_id)
        .await
        .expect("failed to load tasks");

    // Assert
    assert_eq!(orchestration.controller_project_id, 1);
    assert_eq!(orchestration.controller_session_id, "controller");
    assert_eq!(orchestration.max_parallelism, 3);
    assert_eq!(
        orchestration.status,
        OrchestrationStatus::AwaitingApproval.to_string()
    );
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_key, "alpha");
    assert_eq!(tasks[0].child_session_id, None);
    assert_eq!(tasks[0].attempt_count, 0);
    assert_eq!(
        tasks[0].kind,
        OrchestrationTaskKind::Implementation.to_string()
    );
    assert_eq!(tasks[0].touched_areas, r#"["crates/alpha/"]"#);
    assert!(tasks[0].child_answer.is_none());
    assert!(tasks[0].research_report.is_none());
}

#[tokio::test]
async fn research_task_round_trips_report_and_latest_child_answer_without_scope() {
    // Arrange
    let database = controller_fixture().await;
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert orchestration");
    let task_id = database
        .orchestrations()
        .upsert_orchestration_task(PersistedOrchestrationTask {
            kind: OrchestrationTaskKind::Research.to_string(),
            touched_areas: "[]".to_string(),
            ..planned_task(orchestration_id, "architecture")
        })
        .await
        .expect("failed to insert research task");
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(task_id)
            .await
            .expect("failed to claim research task")
    );
    database
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: "research-child",
            is_draft: false,
            model: AgentKind::Codex.default_model().as_str(),
            orchestration_task_id: Some(task_id),
            parent_session_id: None,
            permission_mode: ag_agent::PermissionMode::AutoEdit,
            personality_id: None,
            project_id: 1,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: Some("OrchestrationResearcher"),
            speed_mode: SpeedMode::Normal,
            status: "Review",
        })
        .await
        .expect("failed to insert research child");
    assert!(
        database
            .orchestrations()
            .link_orchestration_task_child(task_id, "research-child")
            .await
            .expect("failed to link research child")
    );
    database
        .sessions()
        .append_session_message(
            "research-child",
            SessionMessageKind::AssistantAnswer,
            "Full architecture report",
        )
        .await
        .expect("failed to persist research answer");

    // Act
    database
        .orchestrations()
        .update_orchestration_task_research_report(task_id, "Full architecture report")
        .await
        .expect("failed to persist research report");
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration_id)
        .await
        .expect("failed to load research task")
        .remove(0);
    let scope = database
        .orchestrations()
        .load_orchestration_task_scope_for_child("research-child")
        .await
        .expect("failed to inspect research child scope");

    // Assert
    assert_eq!(task.kind, OrchestrationTaskKind::Research.to_string());
    assert_eq!(
        task.child_answer.as_deref(),
        Some("Full architecture report")
    );
    assert_eq!(
        task.research_report.as_deref(),
        Some("Full architecture report")
    );
    assert!(scope.is_none());
}

#[tokio::test]
async fn orchestration_task_kind_validation_rejects_unknown_writes_and_hydration() {
    // Arrange
    let (database, pool) = controller_fixture_with_pool().await;
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert orchestration");
    let invalid_task = PersistedOrchestrationTask {
        kind: "Unknown".to_string(),
        ..planned_task(orchestration_id, "invalid-write")
    };

    // Act
    let write_error = database
        .orchestrations()
        .upsert_orchestration_task(invalid_task)
        .await
        .expect_err("unknown task kind should be rejected before persistence");
    sqlx::query(
        "INSERT INTO session_orchestration_task (session_orchestration_id, task_key, title, \
         prompt, status, kind) VALUES (?, 'invalid-read', 'Invalid', 'Inspect', 'Planned', \
         'Unknown')",
    )
    .bind(orchestration_id)
    .execute(&pool)
    .await
    .expect("failed to seed invalid persisted kind");
    let read_error = database
        .orchestrations()
        .load_orchestration_tasks(orchestration_id)
        .await
        .expect_err("unknown persisted task kind should fail hydration");

    // Assert
    assert!(matches!(
        write_error,
        DbError::InvalidData {
            entity: "orchestration task kind",
            reason,
        } if reason == "unknown persisted kind `Unknown`"
    ));
    assert!(matches!(
        read_error,
        DbError::InvalidData {
            entity: "orchestration task kind",
            reason,
        } if reason == "unknown persisted kind `Unknown`"
    ));
}

#[tokio::test]
/// Reuses the same row when a retry re-proposes an existing task key, so a
/// respawn cannot fan out a duplicate child for one subtask.
async fn test_retry_with_same_task_key_updates_the_existing_row() {
    // Arrange
    let database = controller_fixture().await;
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to load project");
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert orchestration");
    let first_id = database
        .orchestrations()
        .upsert_orchestration_task(planned_task(orchestration_id, "alpha"))
        .await
        .expect("failed to insert task");
    let first_claim = database
        .orchestrations()
        .claim_orchestration_task(first_id)
        .await
        .expect("failed to claim first attempt");
    insert_orchestration_child(&database, project_id, "child-old", first_id).await;
    let first_link = database
        .orchestrations()
        .link_orchestration_task_child(first_id, "child-old")
        .await
        .expect("failed to link old child");
    database
        .orchestrations()
        .update_orchestration_task_status(
            first_id,
            &OrchestrationTaskStatus::Failed.to_string(),
            Some("agent crashed".to_string()),
        )
        .await
        .expect("failed to fail task");

    // Act
    let retried_id = database
        .orchestrations()
        .upsert_orchestration_task(PersistedOrchestrationTask {
            title: "Task alpha, retried".to_string(),
            ..planned_task(orchestration_id, "alpha")
        })
        .await
        .expect("failed to retry task");
    let detached_child = database
        .orchestrations()
        .load_child_session_id_for_task(first_id)
        .await
        .expect("failed to check detached child");
    let retry_claim = database
        .orchestrations()
        .claim_orchestration_task(retried_id)
        .await
        .expect("failed to claim replacement attempt");
    insert_orchestration_child(&database, project_id, "child-replacement", retried_id).await;
    let replacement_link = database
        .orchestrations()
        .link_orchestration_task_child(retried_id, "child-replacement")
        .await
        .expect("failed to link replacement child");
    let linked_child = database
        .orchestrations()
        .load_child_session_id_for_task(first_id)
        .await
        .expect("failed to check replacement child");
    let tasks = database
        .orchestrations()
        .load_orchestration_tasks(orchestration_id)
        .await
        .expect("failed to load tasks");

    // Assert
    assert!(first_claim);
    assert!(first_link);
    assert!(retry_claim);
    assert!(replacement_link);
    assert_eq!(retried_id, first_id);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].title, "Task alpha, retried");
    assert_eq!(
        tasks[0].status,
        OrchestrationTaskStatus::Running.to_string()
    );
    assert_eq!(tasks[0].attempt_count, 2);
    assert_eq!(
        tasks[0].child_session_id.as_deref(),
        Some("child-replacement")
    );
    assert_eq!(tasks[0].last_error, None);
    assert_eq!(detached_child, None);
    assert_eq!(linked_child.as_deref(), Some("child-replacement"));
}

#[tokio::test]
/// Links a created child, counts the attempt, and exposes its observed
/// session state through the task snapshot.
async fn test_child_linkage_counts_attempts_and_loads_observed_state() {
    // Arrange
    let database = controller_fixture().await;
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("child-a", "gpt-5.6-sol", "main", "Draft", project_id)
        .await
        .expect("failed to insert child session");
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
    let claimed = database
        .orchestrations()
        .claim_orchestration_task(task_id)
        .await
        .expect("failed to claim task");

    // Act
    let linked = database
        .orchestrations()
        .link_orchestration_task_child(task_id, "child-a")
        .await
        .expect("failed to link child");
    database
        .orchestrations()
        .update_orchestration_task_result_summary(task_id, "Added the parser")
        .await
        .expect("failed to record summary");
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration_id)
        .await
        .expect("failed to load task")
        .remove(0);

    // Assert
    assert!(claimed);
    assert!(linked);
    assert_eq!(task.id, task_id);
    assert_eq!(task.attempt_count, 1);
    assert_eq!(task.child_session_id.as_deref(), Some("child-a"));
    assert_eq!(task.result_summary.as_deref(), Some("Added the parser"));
    assert_eq!(task.status, OrchestrationTaskStatus::Running.to_string());
    assert_eq!(task.child_status.as_deref(), Some("Draft"));
    assert_eq!(task.child_input_tokens, 0);
    assert_eq!(task.child_output_tokens, 0);
}

#[tokio::test]
/// Bulk-loads controller progress and child adjacency without per-session
/// orchestration queries.
async fn test_session_metadata_for_project_loads_controller_and_children_together() {
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
    let running_task_id = database
        .orchestrations()
        .upsert_orchestration_task(planned_task(orchestration_id, "running"))
        .await
        .expect("failed to insert running task");
    let waiting_task_id = database
        .orchestrations()
        .upsert_orchestration_task(planned_task(orchestration_id, "waiting"))
        .await
        .expect("failed to insert waiting task");
    insert_orchestration_child(&database, project_id, "child-running", running_task_id).await;
    insert_orchestration_child(&database, project_id, "child-waiting", waiting_task_id).await;
    let running_claim = database
        .orchestrations()
        .claim_orchestration_task(running_task_id)
        .await
        .expect("failed to claim running task");
    let waiting_claim = database
        .orchestrations()
        .claim_orchestration_task(waiting_task_id)
        .await
        .expect("failed to claim waiting task");
    let running_link = database
        .orchestrations()
        .link_orchestration_task_child(running_task_id, "child-running")
        .await
        .expect("failed to link running child");
    let waiting_link = database
        .orchestrations()
        .link_orchestration_task_child(waiting_task_id, "child-waiting")
        .await
        .expect("failed to link waiting child");
    database
        .orchestrations()
        .update_orchestration_task_status(
            waiting_task_id,
            &OrchestrationTaskStatus::WaitingForInput.to_string(),
            None,
        )
        .await
        .expect("failed to mark waiting child");

    // Act
    let metadata = database
        .orchestrations()
        .load_session_metadata_for_project(project_id)
        .await
        .expect("failed to load bulk orchestration metadata");

    // Assert
    assert!(running_claim);
    assert!(waiting_claim);
    assert!(running_link);
    assert!(waiting_link);
    assert_eq!(
        metadata,
        vec![
            SessionOrchestrationMetadataRow {
                controller_session_id: Some("controller".to_string()),
                orchestration_status: None,
                running_task_count: 0,
                session_id: "child-running".to_string(),
                waiting_task_count: 0,
            },
            SessionOrchestrationMetadataRow {
                controller_session_id: Some("controller".to_string()),
                orchestration_status: None,
                running_task_count: 0,
                session_id: "child-waiting".to_string(),
                waiting_task_count: 0,
            },
            SessionOrchestrationMetadataRow {
                controller_session_id: None,
                orchestration_status: Some(OrchestrationStatus::Running.to_string()),
                running_task_count: 1,
                session_id: "controller".to_string(),
                waiting_task_count: 1,
            },
        ]
    );
}

#[tokio::test]
/// Returns no orchestration for a controller session that never planned.
async fn test_missing_orchestration_returns_none() {
    // Arrange
    let database = controller_fixture().await;

    // Act
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load orchestration");
    // Assert
    assert_eq!(orchestration, None);
}
