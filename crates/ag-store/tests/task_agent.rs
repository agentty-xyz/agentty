//! Durable task context contract through the public SQLite adapter.

use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, DurableMessageVisibility,
    SubmitAgentTaskRequest, TaskAgentAssignment, TaskAgentInput,
};
use ag_store::Database;
use tempfile::tempdir;

#[tokio::test]
async fn reopening_preserves_task_assignment_context_and_delivery_identity() {
    // Arrange
    let directory = tempdir().expect("temporary database directory");
    let path = directory.path().join("task.sqlite");
    let database = Database::open(&path).await.expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await
        .expect("session");
    let request = SubmitAgentTaskRequest {
        assignment: TaskAgentAssignment {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            definition: BuiltinAgentDefinition::Planner,
        },
        attempt_id: "first".to_string(),
        input: TaskAgentInput {
            accepted_plan: None,
            clarification: None,
            user_request: "Plan this change".to_string(),
        },
        objective: "Produce a plan".to_string(),
        task_id: "task".to_string(),
        visibility: DurableMessageVisibility::Visible,
    };
    database
        .tasks()
        .claim_agent_attempt("session", &request, "reply")
        .await
        .expect("admit");
    let original = database
        .tasks()
        .load_agent_context("task")
        .await
        .expect("load")
        .expect("context");
    database
        .operations()
        .mark_session_operation_failed("task-attempt:first", "interrupted")
        .await
        .expect("settle");
    database.pool().close().await;

    // Act
    let reopened = Database::open(&path).await.expect("reopen");
    let retained = reopened
        .tasks()
        .load_agent_context("task")
        .await
        .expect("load")
        .expect("context");
    let existing = reopened
        .tasks()
        .contains_agent_attempt("session", &request)
        .await
        .expect("delivery");
    let admitted = reopened
        .tasks()
        .claim_agent_attempt("session", &request, "reply")
        .await
        .expect("redelivery");

    // Assert
    assert_eq!(retained, original);
    assert!(existing);
    assert!(!admitted);
    assert_eq!(
        reopened
            .tasks()
            .load_task("task")
            .await
            .expect("task")
            .expect("present")
            .attempts
            .len(),
        1
    );
}
