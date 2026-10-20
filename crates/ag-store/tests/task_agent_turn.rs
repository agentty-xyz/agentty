//! Public task-history durability and idempotent response persistence.

use ag_session::{
    AcceptTaskPlanRequest, AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition,
    DurableMessageVisibility, SubmitAgentTaskRequest, TaskAgentAssignment, TaskAgentInput,
};
use ag_store::{Database, DbError, TaskAgentTurnMetadata};
use tempfile::tempdir;

#[tokio::test]
async fn reopening_retains_task_only_continuation_and_exact_response() {
    // Arrange
    let directory = tempdir().expect("database directory");
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
    let request = planner_request();
    database
        .tasks()
        .claim_agent_attempt("session", &request, "reply")
        .await
        .expect("admit");
    database
        .operations()
        .mark_session_operation_running("task-attempt:first")
        .await
        .expect("running");
    let metadata = TaskAgentTurnMetadata {
        assistant_message: "\n{\"answer\":\"A plan\",\"questions\":[]}\n".to_string(),
        harness_turn_report: None,
        instruction_conversation_id: Some("task-bootstrap".to_string()),
        input_tokens: 5,
        output_tokens: 2,
        provider_conversation_id: Some("task-thread".to_string()),
    };
    database
        .tasks()
        .persist_agent_turn("first", &metadata)
        .await
        .expect("persist response");
    database
        .operations()
        .mark_session_operation_done("task-attempt:first")
        .await
        .expect("settle");
    let original = database
        .tasks()
        .load_agent_history("task")
        .await
        .expect("history");
    database.pool().close().await;

    // Act
    let reopened = Database::open(&path).await.expect("reopen");
    reopened
        .tasks()
        .persist_agent_turn("first", &metadata)
        .await
        .expect("idempotent saved response");
    let history = reopened
        .tasks()
        .load_agent_history("task")
        .await
        .expect("history")
        .expect("context");

    // Assert
    assert_eq!(Some(history.clone()), original);
    assert_eq!(
        reopened
            .sessions()
            .get_session_provider_conversation_id("session")
            .await
            .expect("ordinary context"),
        None
    );
    let usage: (i64, i64, i64) = sqlx::query_as(
        "SELECT input_tokens, output_tokens, invocation_count FROM session_usage WHERE session_id \
         = 'session'",
    )
    .fetch_one(reopened.pool())
    .await
    .expect("usage");
    assert_eq!(usage, (5, 2, 1));

    // Act: explicitly accept the reopened exact Planner version and hand it
    // off.
    assert_reopened_plan_handoff(&reopened, request)
        .await
        .expect("reopened plan handoff");
}

async fn assert_reopened_plan_handoff(
    reopened: &Database,
    request: SubmitAgentTaskRequest,
) -> Result<(), DbError> {
    // Arrange and Act
    let plan = reopened
        .tasks()
        .accept_agent_plan(
            "session",
            &AcceptTaskPlanRequest {
                task_id: "task".into(),
                attempt_id: "first".into(),
            },
        )
        .await?;
    assert_eq!(plan.text, "A plan");
    let mut implementation = request;
    implementation.assignment.definition = BuiltinAgentDefinition::Implementer;
    implementation.task_id = "implementation".into();
    implementation.attempt_id = "implement".into();
    implementation.input.accepted_plan = Some(plan);
    let admitted = reopened
        .tasks()
        .claim_agent_attempt("session", &implementation, "reply")
        .await?;

    // Assert
    assert!(admitted);

    Ok(())
}

fn planner_request() -> SubmitAgentTaskRequest {
    SubmitAgentTaskRequest {
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
        objective: "A concrete plan".to_string(),
        task_id: "task".to_string(),
        visibility: DurableMessageVisibility::Visible,
    }
}
