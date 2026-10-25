//! Public durability contract for graph snapshots, authorities, and handoffs.

use std::error::Error;
use std::num::NonZeroU64;
use std::sync::Arc;

use ag_protocol::{AgentResponse, ProtocolRequestProfile, TurnPrompt, TurnPromptAttachment};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, DurableMessageVisibility, EnvId,
    ExecutionGraph, GraphCompletionGate, GraphContext, GraphDependency, GraphExecutionProfile,
    GraphExecutionRef, GraphInputBinding, GraphNode, GraphOutput, GraphPostProcessing,
    GraphResolvedInput, GraphResultRef, GraphResultState, GraphWorkspaceAccess, SubmitTaskRequest,
};
use ag_store::{Database, GraphEvidence, GraphExecutionRecord};
use ag_worker::{RunInfo, RunState};

#[tokio::test]
async fn explicit_graph_continuation_and_cancellation_survive_reopening_without_replay()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::Builder::new()
        .prefix("graph-lifecycle-")
        .tempdir_in(".")?;
    let path = directory.path().join("graph.sqlite");
    let (database, snapshot) = open_lifecycle_graph(&path).await?;
    let admission = database
        .graph_dispatch()
        .claim(
            "graph",
            snapshot.revision,
            "plan",
            &uuid::Uuid::new_v4().to_string(),
            std::path::Path::new("workspace"),
        )
        .await?;
    let dispatch = match admission {
        ag_worker::GraphAdmission::Claimed(dispatch) => Some(dispatch),
        _ => None,
    }
    .expect("claim");
    database
        .graph_dispatch()
        .fail(&dispatch.record, "Interrupted by application restart")
        .await?;
    let request = ag_session::ContinueGraphNodeRequest {
        action_id: uuid::Uuid::new_v4().to_string(),
        execution: dispatch.record.execution.clone(),
        graph_id: "graph".into(),
        kind: ag_session::GraphNodeContinuation::Retry,
        node_id: "plan".into(),
        revision: snapshot.revision,
    };
    database.graphs().continue_node(&request).await?;
    database.pool().close().await;
    drop(database);

    // Act: reopening projects pending intent but never creates an attempt.
    let reopened = Database::open(&path).await?;
    let progress = reopened.graphs().progress("graph").await?.expect("graph");

    // Assert
    assert_eq!(
        progress.nodes[0].status,
        ag_session::GraphNodeStatus::Pending
    );
    assert_eq!(
        progress.nodes[0].executions,
        vec![dispatch.record.execution.clone()]
    );
    assert!(!reopened.graphs().continue_node(&request).await?);
    let admission = reopened
        .graph_dispatch()
        .claim(
            "graph",
            snapshot.revision,
            "plan",
            "ignored-redelivery-id",
            std::path::Path::new("workspace"),
        )
        .await?;
    let retry = match admission {
        ag_worker::GraphAdmission::Claimed(dispatch) => Some(dispatch),
        _ => None,
    }
    .expect("explicit dispatch");
    assert_ne!(retry.record.execution, dispatch.record.execution);
    assert!(
        matches!(&retry.record.execution, GraphExecutionRef::TaskAttempt { attempt_id, .. } if attempt_id == &request.action_id)
    );
    reopened
        .graphs()
        .cancel(&ag_session::GraphControlRequest {
            graph_id: "graph".into(),
            revision: snapshot.revision,
        })
        .await?;
    reopened.pool().close().await;
    drop(reopened);
    let reopened = Database::open(&path).await?;
    assert_eq!(
        reopened
            .graphs()
            .progress("graph")
            .await?
            .expect("graph")
            .status,
        ag_session::ExecutionGraphStatus::Cancelled
    );
    assert!(matches!(
        reopened
            .graph_dispatch()
            .claim(
                "graph",
                snapshot.revision,
                "review",
                "never-dispatched",
                std::path::Path::new("workspace")
            )
            .await?,
        ag_worker::GraphAdmission::Blocked
    ));
    reopened.pool().close().await;

    Ok(())
}

async fn open_lifecycle_graph(
    path: &std::path::Path,
) -> Result<(Database, ExecutionGraph), Box<dyn Error>> {
    let database = Database::open(path).await?;
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await?;
    let snapshot = graph();
    database.graphs().save_graph(&snapshot, None).await?;

    Ok((database, snapshot))
}

#[tokio::test]
async fn competing_revision_writes_have_one_winner() {
    // Arrange
    let directory = tempfile::Builder::new()
        .prefix("graph-race-")
        .tempdir_in(".")
        .expect("directory");
    let database = Database::open(&directory.path().join("graph.sqlite"))
        .await
        .expect("database");
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
    let original = graph();
    database
        .graphs()
        .save_graph(&original, None)
        .await
        .expect("original");
    let mut first = original.clone();
    first.revision = NonZeroU64::new(2).expect("revision");
    first.nodes[0].objective = "First proposal".into();
    let mut second = first.clone();
    second.nodes[0].objective = "Second proposal".into();

    // Act
    let (first_result, second_result) = tokio::join!(
        database
            .graphs()
            .save_graph(&first, Some(original.revision)),
        database
            .graphs()
            .save_graph(&second, Some(original.revision))
    );

    // Assert
    assert_ne!(first_result.is_ok(), second_result.is_ok());
    let winner = if first_result.is_ok() { first } else { second };
    assert_eq!(
        database
            .graphs()
            .load_graph("graph", None)
            .await
            .expect("head"),
        Some(winner)
    );
    assert_eq!(
        database
            .graphs()
            .load_graph("graph", Some(original.revision))
            .await
            .expect("history"),
        Some(original)
    );
    database.pool().close().await;
}

#[tokio::test]
async fn graph_handoffs_survive_reopening_without_retargeting_or_replaying_execution()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::Builder::new()
        .prefix("graph-contract-")
        .tempdir_in(".")?;
    let path = directory.path().join("graph.sqlite");
    let database = Database::open_with_timestamp_source(&path, Arc::new(|| 123)).await?;
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await?;
    let graph = graph();
    assert!(database.graphs().save_graph(&graph, None).await?);
    let source = admit_task(&database, "attempt-1").await?;
    let reference =
        produce_plan(&database, &source, "plan-v1", NonZeroU64::MIN, "Exact plan").await?;
    accept_plan(&database, &reference).await?;
    let consumer = link_consumer(&database, &reference).await?;
    let retry = admit_task(&database, "attempt-2").await?;
    let newer = produce_plan(
        &database,
        &retry,
        "plan-v2",
        NonZeroU64::MIN.saturating_add(1),
        "Revised plan",
    )
    .await?;
    database.pool().close().await;
    drop(database);

    // Act
    let reopened = Database::open_with_timestamp_source(&path, Arc::new(|| 999)).await?;

    // Assert
    assert_eq!(
        reopened.graphs().load_graph(&graph.id, None).await?,
        Some(graph.clone())
    );
    assert_eq!(
        reopened
            .graphs()
            .load_execution(&consumer.execution)
            .await?,
        Some(consumer.clone())
    );
    assert_eq!(
        reopened
            .graphs()
            .list_executions("graph", graph.revision)
            .await?,
        vec![source, consumer.clone(), retry]
    );
    assert_durable_results(&reopened, &reference, &newer).await?;
    assert_eq!(
        reopened
            .graphs()
            .list_results("graph", "review", "response")
            .await?,
        Vec::new()
    );
    assert_eq!(
        reopened
            .graphs()
            .list_executions("missing", graph.revision)
            .await?,
        Vec::new()
    );
    assert!(!reopened.graphs().link_execution(&consumer).await?);
    assert!(!reopened.graphs().save_graph(&graph, None).await?);
    assert!(
        !reopened
            .graphs()
            .accept_result(&reference, &acceptance())
            .await?
    );
    assert!(
        reopened
            .tasks()
            .load_task("task")
            .await?
            .expect("task")
            .completed_at
            .is_none()
    );
    let status: String = sqlx::query_scalar("SELECT status FROM agent_run WHERE id = 'review-run'")
        .fetch_one(reopened.pool())
        .await?;
    assert_eq!(status, RunState::Queued.as_str());
    reopened.pool().close().await;

    Ok(())
}

async fn admit_task(
    database: &Database,
    attempt: &str,
) -> Result<GraphExecutionRecord, Box<dyn Error>> {
    let request = SubmitTaskRequest {
        attempt_id: attempt.into(),
        message: "Prepare a plan".into(),
        objective: "Plan".into(),
        task_id: "task".into(),
        visibility: DurableMessageVisibility::Hidden,
    };
    database
        .tasks()
        .claim_attempt("session", &request, "chat")
        .await?;
    let source = GraphExecutionRecord {
        execution: GraphExecutionRef::TaskAttempt {
            attempt_id: request.attempt_id.clone(),
            operation_id: request.operation_id(),
            task_id: request.task_id.clone(),
        },
        graph_id: "graph".into(),
        graph_revision: NonZeroU64::MIN,
        inputs: Vec::new(),
        node_id: "plan".into(),
    };
    database.graphs().link_execution(&source).await?;
    database
        .operations()
        .mark_session_operation_running(&request.operation_id())
        .await?;
    database
        .operations()
        .mark_session_operation_done(&request.operation_id())
        .await?;

    Ok(source)
}

async fn produce_plan(
    database: &Database,
    source: &GraphExecutionRecord,
    id: &str,
    version: NonZeroU64,
    text: &str,
) -> Result<GraphResultRef, Box<dyn Error>> {
    let reference = GraphResultRef {
        execution: source.execution.clone(),
        graph_id: source.graph_id.clone(),
        graph_revision: source.graph_revision,
        id: id.into(),
        node_id: source.node_id.clone(),
        output_name: "response".into(),
        schema: ProtocolRequestProfile::SessionTurn,
        version,
    };
    database
        .graphs()
        .produce_result(
            &reference,
            &serde_json::json!(AgentResponse::plain(text)).to_string(),
        )
        .await?;

    Ok(reference)
}

async fn accept_plan(
    database: &Database,
    reference: &GraphResultRef,
) -> Result<(), Box<dyn Error>> {
    database
        .graphs()
        .validate_result(
            reference,
            &GraphEvidence {
                actor: "schema-validator".into(),
                reason: "Checked complete plan".into(),
            },
        )
        .await?;
    database
        .graphs()
        .accept_result(reference, &acceptance())
        .await?;

    Ok(())
}

async fn link_consumer(
    database: &Database,
    reference: &GraphResultRef,
) -> Result<GraphExecutionRecord, Box<dyn Error>> {
    database
        .runs()
        .create(&RunInfo {
            folder: "workspace".into(),
            id: "review-run".into(),
            parent_id: None,
            project_id: None,
            purpose: "review".into(),
            session_id: Some("session".into()),
        })
        .await?;
    let consumer = GraphExecutionRecord {
        execution: GraphExecutionRef::UtilityRun {
            run_id: "review-run".into(),
        },
        graph_id: "graph".into(),
        graph_revision: NonZeroU64::MIN,
        inputs: vec![GraphResolvedInput {
            name: "plan".into(),
            result: reference.clone(),
        }],
        node_id: "review".into(),
    };
    database.graphs().link_execution(&consumer).await?;

    Ok(consumer)
}

async fn assert_durable_results(
    database: &Database,
    original: &GraphResultRef,
    newer: &GraphResultRef,
) -> Result<(), Box<dyn Error>> {
    let versions = database
        .graphs()
        .list_results("graph", "plan", "response")
        .await?;
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].result.reference, *original);
    assert_eq!(versions[0].result.state, GraphResultState::Accepted);
    assert_eq!(
        versions[0].body,
        serde_json::json!(AgentResponse::plain("Exact plan")).to_string()
    );
    assert_eq!(
        versions[0]
            .validation
            .as_ref()
            .map(|value| value.recorded_at),
        Some(123)
    );
    assert_eq!(
        versions[0].acceptance.as_ref().map(|value| &value.evidence),
        Some(&acceptance())
    );
    assert_eq!(versions[1].result.reference, *newer);
    assert_eq!(versions[1].result.state, GraphResultState::Produced);

    Ok(())
}

fn acceptance() -> GraphEvidence {
    GraphEvidence {
        actor: "user".into(),
        reason: "Accept plan-v1".into(),
    }
}

fn graph() -> ExecutionGraph {
    let plan = GraphNode {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        definition: AgentDefinition {
            description: "Plan".into(),
            id: "planner".into(),
            name: "Planner".into(),
            prompt: "Prepare a plan".into(),
            read_only: true,
        },
        dependencies: Vec::new(),
        env_id: EnvId::SessionWorktree("session".into()),
        execution: GraphExecutionProfile {
            context: GraphContext::TaskConversation {
                task_id: "task".into(),
            },
            post_processing: GraphPostProcessing::None,
            response_profile: ProtocolRequestProfile::SessionTurn,
            shared_budget_id: None,
            workspace_access: GraphWorkspaceAccess::SharedRead,
        },
        id: "plan".into(),
        inputs: Vec::new(),
        objective: "Plan".into(),
        outputs: vec![GraphOutput {
            name: "response".into(),
            schema: ProtocolRequestProfile::SessionTurn,
        }],
        request: TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                local_image_path: "design.png".into(),
                placeholder: "[Image #1]".into(),
            }],
            ..TurnPrompt::from_agent_data("Design evidence".into())
        },
    };
    let mut review = plan.clone();
    review.id = "review".into();
    review.execution.context = GraphContext::Fresh;
    review.execution.response_profile = ProtocolRequestProfile::FocusedReview;
    review.execution.shared_budget_id = Some("review-budget".into());
    review.outputs[0].schema = ProtocolRequestProfile::FocusedReview;
    review.dependencies = vec![GraphDependency {
        node_id: plan.id.clone(),
        gate: GraphCompletionGate::ResultAccepted {
            output_name: "response".into(),
        },
    }];
    review.inputs = vec![GraphInputBinding {
        name: "plan".into(),
        output_name: "response".into(),
        producer_node_id: plan.id.clone(),
    }];

    ExecutionGraph {
        id: "graph".into(),
        nodes: vec![plan, review],
        revision: NonZeroU64::MIN,
        session_id: "session".into(),
    }
}
