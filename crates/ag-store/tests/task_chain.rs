//! Public durability of the graph-backed plan workflow and reply decisions.

use std::error::Error;
use std::path::Path;

use ag_protocol::TurnPrompt;
use ag_session::{
    AgentKind, AgentModel, AgentSelection, CreateTaskRequest, DurableMessageVisibility,
    ExecutionGraph, GraphExecutionRef, PlanThenImplementAgents, StartPlanThenImplementRequest,
    SubmitTaskRequest, TaskChainPhase, TaskChainReplyOutcome,
};
use ag_store::{Database, GraphEvidence, TaskAgentTurnMetadata};
use ag_worker::{GraphAdmission, GraphDispatch};

#[tokio::test]
async fn reopening_retains_the_accepted_graph_handoff_and_original_reply_decision()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("workflow.sqlite");
    let database = Database::open(&path).await?;
    let (graph, planner) = planned_workflow(&database).await?;
    for role in ["planner", "implementer"] {
        reject_ordinary_task_inputs(&database, &format!("chain:workflow:{role}")).await?;
    }
    database
        .graphs()
        .record_task_chain_reply(
            &graph.id,
            "approval",
            &TurnPrompt::from_text("Go ahead".into()),
            &planner.record.execution,
        )
        .await?;
    let decision = database
        .graphs()
        .decide_task_chain_reply("approval", TaskChainReplyOutcome::PlanAccepted, None)
        .await?;
    let plan = database
        .graphs()
        .list_results(&graph.id, "planner", "plan")
        .await?
        .pop()
        .expect("plan")
        .result
        .reference;
    database
        .graphs()
        .accept_result(
            &plan,
            &GraphEvidence {
                actor: "user".into(),
                reason: "Go ahead".into(),
            },
        )
        .await?;
    let implementation = match database
        .graph_dispatch()
        .claim(
            &graph.id,
            graph.revision,
            "implementer",
            &uuid::Uuid::new_v4().to_string(),
            Path::new("workspace"),
        )
        .await?
    {
        GraphAdmission::Claimed(dispatch) => Some(dispatch),
        _ => None,
    }
    .expect("implementation admission");
    database
        .graph_dispatch()
        .fail(&implementation.record, "Interrupted by restart")
        .await?;
    database.pool().close().await;

    // Act
    let reopened = Database::open(&path).await?;
    reject_ordinary_task_inputs(&reopened, "chain:workflow:implementer").await?;
    let chains = reopened.graphs().list_task_chains("session").await?;
    let redelivery = reopened
        .graph_dispatch()
        .claim(
            &graph.id,
            graph.revision,
            "implementer",
            &uuid::Uuid::new_v4().to_string(),
            Path::new("workspace"),
        )
        .await?;
    let reply = reopened
        .graphs()
        .decide_task_chain_reply(
            "approval",
            TaskChainReplyOutcome::PlanRevised,
            Some(uuid::Uuid::new_v4().to_string()),
        )
        .await?;

    // Assert
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].phase, TaskChainPhase::Failed);
    assert_eq!(
        chains[0].accepted_plan.as_ref().expect("accepted").text,
        "Build a title index"
    );
    assert_eq!(reply, decision);
    assert_eq!(implementation.inputs[0].reference.result, plan);
    assert!(
        matches!(redelivery, GraphAdmission::Existing(record) if record == implementation.record)
    );
    assert_eq!(
        reopened.graphs().load_graph(&graph.id, None).await?,
        Some(graph)
    );
    reopened.pool().close().await;

    Ok(())
}

/// Public task APIs cannot occupy a role's durable graph reservation.
async fn reject_ordinary_task_inputs(
    database: &Database,
    task_id: &str,
) -> Result<(), Box<dyn Error>> {
    let before = database.tasks().list_tasks("session").await?;
    let created = database
        .tasks()
        .create_task(
            "session",
            &CreateTaskRequest {
                objective: "Add search".into(),
                task_id: task_id.into(),
            },
        )
        .await;
    let submitted = database
        .tasks()
        .claim_attempt(
            "session",
            &SubmitTaskRequest {
                attempt_id: "ordinary-attempt".into(),
                message: "Occupy the workflow task".into(),
                objective: "Add search".into(),
                task_id: task_id.into(),
                visibility: DurableMessageVisibility::Visible,
            },
            "reply",
        )
        .await;
    for result in [created.map(|()| true), submitted] {
        assert!(matches!(
            result,
            Err(ag_store::DbError::InvalidData { reason, .. })
                if reason == "Workflow task identities require graph admission"
        ));
    }
    assert_eq!(database.tasks().list_tasks("session").await?, before);

    Ok(())
}

async fn planned_workflow(
    database: &Database,
) -> Result<(ExecutionGraph, Box<GraphDispatch>), Box<dyn Error>> {
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await?;
    let graph = StartPlanThenImplementRequest {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        agents: Box::new(PlanThenImplementAgents::bundled().ok_or("bundled agents missing")?),
        attachments: Vec::new(),
        chain_id: "workflow".into(),
        user_request: "Add search".into(),
    }
    .graph(&"session".into(), Some("User: use the title index"));
    database
        .graphs()
        .start_task_chain(&graph, "Add search")
        .await?;
    let planner = match database
        .graph_dispatch()
        .claim(
            &graph.id,
            graph.revision,
            "planner",
            &uuid::Uuid::new_v4().to_string(),
            Path::new("workspace"),
        )
        .await?
    {
        GraphAdmission::Claimed(dispatch) => Some(dispatch),
        _ => None,
    }
    .ok_or("planner admission missing")?;
    let operation_id = match &planner.record.execution {
        GraphExecutionRef::TaskAttempt { operation_id, .. } => Some(operation_id),
        GraphExecutionRef::UtilityRun { .. } => None,
    }
    .ok_or("task attempt missing")?;
    let body = r#"{"answer":"Build a title index"}"#;
    database
        .operations()
        .mark_session_operation_running(operation_id)
        .await?;
    database
        .graphs()
        .record_task_turn(
            &planner,
            &TaskAgentTurnMetadata {
                assistant_message: body.into(),
                harness_turn_report: None,
                instruction_conversation_id: None,
                input_tokens: 1,
                output_tokens: 1,
                provider_conversation_id: None,
            },
        )
        .await?;
    database
        .operations()
        .mark_session_operation_done(operation_id)
        .await?;
    database.graph_dispatch().finish(&planner, body).await?;

    Ok((graph, planner))
}
