use std::num::NonZeroU64;
use std::path::Path;

use ag_session::{
    GraphCompletionGate, GraphContext, GraphDependency, GraphExecutionRef, GraphPostProcessing,
    GraphWorkspaceAccess,
};
use ag_worker::{GraphAdmission, GraphDispatch, GraphExecutionRecord, RunState};

use crate::execution_graph::support_test::{evidence, graph, reader, run};
use crate::{Database, GraphEvidence};

async fn database() -> Database {
    let database = crate::test_support::task_database_fixture().await;
    database
        .graphs()
        .save_graph(&graph(), None)
        .await
        .expect("graph");

    database
}

async fn claim(database: &Database, node: &str, id: &str) -> GraphAdmission {
    database
        .graph_dispatch()
        .claim("graph", NonZeroU64::MIN, node, id, Path::new("workspace"))
        .await
        .expect("admission")
}

fn dispatched(admission: GraphAdmission) -> GraphDispatch {
    let dispatch = match admission {
        GraphAdmission::Claimed(dispatch) => Some(*dispatch),
        _ => None,
    };

    dispatch.expect("expected claim")
}

async fn finish(database: &Database, dispatch: &GraphDispatch, body: &str) {
    settle(database, dispatch).await;
    database
        .graph_dispatch()
        .finish(dispatch, body)
        .await
        .expect("response");
}

async fn settle(database: &Database, dispatch: &GraphDispatch) {
    match &dispatch.record.execution {
        GraphExecutionRef::UtilityRun { run_id } => {
            database
                .runs()
                .transition(run_id, RunState::Completed, None)
                .await
                .expect("complete");
        }
        GraphExecutionRef::TaskAttempt { operation_id, .. } => {
            database
                .operations()
                .mark_session_operation_running(operation_id)
                .await
                .expect("running");
            database
                .operations()
                .mark_session_operation_done(operation_id)
                .await
                .expect("done");
        }
    }
}

#[tokio::test]
async fn all_gates_are_checked_and_inputs_are_pinned_with_acceptance_separate() {
    // Arrange
    let database = database().await;
    assert_eq!(
        claim(&database, "second", "premature").await,
        GraphAdmission::Blocked
    );
    let first = dispatched(claim(&database, "first", "source").await);
    assert!(
        database
            .graph_dispatch()
            .finish(&first, r#"{"answer":"too soon"}"#)
            .await
            .is_err()
    );
    finish(&database, &first, r#"{"answer":"exact evidence"}"#).await;
    let result = database
        .graphs()
        .list_results("graph", "first", "response")
        .await
        .expect("results")
        .remove(0);

    // Act
    assert_eq!(
        claim(&database, "second", "unaccepted").await,
        GraphAdmission::Blocked
    );
    database
        .graphs()
        .accept_result(&result.result.reference, &evidence())
        .await
        .expect("accept");
    let second = dispatched(claim(&database, "second", "consumer").await);

    // Assert
    assert_eq!(second.inputs[0].body, r#"{"answer":"exact evidence"}"#);
    assert_eq!(second.record.inputs[0].result, result.result.reference);
    assert_eq!(
        claim(&database, "second", "duplicate").await,
        GraphAdmission::Existing(second.record.clone())
    );
    finish(&database, &second, r#"{"answer":"consumed"}"#).await;
    database
        .graph_dispatch()
        .finish(&second, r#"{"answer":"consumed"}"#)
        .await
        .expect("idempotent settlement");
    assert!(
        database
            .graph_dispatch()
            .finish(&second, r#"{"answer":"changed"}"#)
            .await
            .is_err()
    );
    let mut conflicting = second.clone();
    conflicting.record.node_id = "first".into();
    assert!(
        database
            .graph_dispatch()
            .finish(&conflicting, r#"{"answer":"changed"}"#)
            .await
            .is_err()
    );
    conflicting = second.clone();
    conflicting.node.objective = "not admitted".into();
    assert!(
        database
            .graph_dispatch()
            .finish(&conflicting, r#"{"answer":"changed"}"#)
            .await
            .is_err()
    );
    assert!(
        database
            .graphs()
            .list_results("graph", "second", "response")
            .await
            .expect("results")[0]
            .acceptance
            .is_none()
    );
}

#[tokio::test]
async fn execution_gate_requires_response_evidence_without_questions_even_without_outputs() {
    // Arrange
    for body in [
        r#"{"answer":"done"}"#,
        r#"{"answer":"Need input","questions":[{"text":"Which target?","options":[]}]}"#,
    ] {
        let database = crate::test_support::task_database_fixture().await;
        let mut graph = graph();
        graph.nodes[0].execution.context = GraphContext::TaskConversation {
            task_id: "task".into(),
        };
        graph.nodes[0].execution.response_profile =
            ag_protocol::ProtocolRequestProfile::SessionTurn;
        graph.nodes[0].outputs.clear();
        graph.nodes[1].inputs.clear();
        graph.nodes[1].dependencies = vec![GraphDependency {
            node_id: "first".into(),
            gate: GraphCompletionGate::ExecutionSucceeded,
        }];
        database
            .graphs()
            .save_graph(&graph, None)
            .await
            .expect("graph");
        let first = dispatched(claim(&database, "first", "source").await);
        settle(&database, &first).await;
        assert_eq!(
            claim(&database, "second", "no-response").await,
            GraphAdmission::Blocked
        );

        // Act
        database
            .graph_dispatch()
            .finish(&first, body)
            .await
            .expect("response");
        let admission = claim(&database, "second", "consumer").await;

        // Assert
        assert_eq!(
            matches!(admission, GraphAdmission::Claimed(_)),
            !body.contains("questions")
        );
    }
}

#[tokio::test]
async fn read_branches_share_env_but_writers_and_other_host_work_exclude_them() {
    // Arrange
    let database = crate::test_support::task_database_fixture().await;
    let mut graph = graph();
    graph.nodes = vec![reader("first"), reader("second"), reader("writer")];
    graph.nodes[2].execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
    database
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("graph");
    run(&database, "host", Some("session"), RunState::Queued).await;
    assert_eq!(
        claim(&database, "first", "blocked").await,
        GraphAdmission::Blocked
    );
    database
        .runs()
        .transition("host", RunState::Completed, None)
        .await
        .expect("host settled");

    // Act
    let first = dispatched(claim(&database, "first", "source").await);
    let second = dispatched(claim(&database, "second", "peer").await);

    // Assert
    assert_eq!(
        claim(&database, "writer", "blocked").await,
        GraphAdmission::Blocked
    );
    finish(&database, &first, r#"{"answer":"done"}"#).await;
    finish(&database, &second, r#"{"answer":"done"}"#).await;
    let writer = dispatched(claim(&database, "writer", "writer-run").await);
    let mut other = graph.clone();
    other.id = "other".into();
    other.nodes = vec![reader("other")];
    database
        .graphs()
        .save_graph(&other, None)
        .await
        .expect("other graph");
    assert_eq!(
        database
            .graph_dispatch()
            .claim(
                "other",
                graph.revision,
                "other",
                "blocked",
                Path::new("workspace")
            )
            .await
            .expect("admission"),
        GraphAdmission::Blocked
    );
    database
        .graph_dispatch()
        .fail(&writer.record, "dispatch failed")
        .await
        .expect("failed");
    assert!(matches!(
        database
            .graph_dispatch()
            .claim(
                "other",
                graph.revision,
                "other",
                "other-run",
                Path::new("workspace")
            )
            .await
            .expect("admission"),
        GraphAdmission::Claimed(_)
    ));
    assert_eq!(
        claim(&database, "writer", "retry").await,
        GraphAdmission::Existing(writer.record)
    );
}

#[tokio::test]
async fn task_admission_is_atomic_and_failures_never_reopen_terminal_authorities() {
    // Arrange
    let database = crate::test_support::task_database_fixture().await;
    let mut graph = graph();
    graph.nodes.truncate(1);
    graph.nodes[0].execution.context = GraphContext::TaskConversation {
        task_id: "graph-task".into(),
    };
    graph.nodes[0].execution.response_profile = ag_protocol::ProtocolRequestProfile::SessionTurn;
    graph.nodes[0].execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
    graph.nodes[0].execution.post_processing = GraphPostProcessing::SessionWorkflow;
    graph.nodes[0].definition.read_only = false;
    graph.nodes[0].outputs[0].schema = ag_protocol::ProtocolRequestProfile::SessionTurn;
    database
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("graph");

    // Act
    let first = dispatched(claim(&database, "first", "attempt").await);
    let task = database
        .tasks()
        .load_task("graph-task")
        .await
        .expect("task")
        .expect("exists");
    assert_eq!(task.attempts.len(), 1);
    finish(&database, &first, r#"{"answer":"done"}"#).await;
    database
        .graph_dispatch()
        .fail(&first.record, "late failure")
        .await
        .expect("ignore terminal failure");

    // Assert
    let task = database
        .tasks()
        .load_task("graph-task")
        .await
        .expect("task")
        .expect("exists");
    assert_eq!(
        task.attempts[0].status,
        ag_session::TaskAttemptStatus::Succeeded
    );
    assert!(task.completed_at.is_none());
    let bypass = ag_session::SubmitTaskRequest {
        attempt_id: "legacy-bypass".into(),
        message: "bypass graph".into(),
        objective: task.objective.clone(),
        task_id: task.id.clone(),
        visibility: ag_session::DurableMessageVisibility::Hidden,
    };
    assert!(
        database
            .tasks()
            .claim_attempt("session", &bypass, "chat")
            .await
            .is_err()
    );
    assert_eq!(
        claim(&database, "first", "retry").await,
        GraphAdmission::Existing(first.record)
    );
    let mut other = graph.clone();
    other.id = "other".into();
    database
        .graphs()
        .save_graph(&other, None)
        .await
        .expect("other");
    assert!(
        database
            .graph_dispatch()
            .claim(
                "other",
                graph.revision,
                "first",
                "hijack",
                Path::new("workspace")
            )
            .await
            .is_err()
    );
    assert_eq!(
        database
            .graphs()
            .list_executions("other", graph.revision)
            .await
            .expect("rollback"),
        Vec::<GraphExecutionRecord>::new()
    );
}

#[tokio::test]
async fn invalid_admission_and_settlement_roll_back_without_orphan_records() {
    // Arrange
    let database = database().await;
    let dispatch = database.graph_dispatch();

    // Act / Assert
    for (graph, revision, node, id, folder) in [
        ("missing", NonZeroU64::MIN, "first", "run", "workspace"),
        (
            "graph",
            NonZeroU64::MIN.saturating_add(1),
            "first",
            "run",
            "workspace",
        ),
        ("graph", NonZeroU64::MIN, "missing", "run", "workspace"),
        ("graph", NonZeroU64::MIN, "first", "", "workspace"),
        ("graph", NonZeroU64::MIN, "first", "run", ""),
    ] {
        assert!(
            dispatch
                .claim(graph, revision, node, id, Path::new(folder))
                .await
                .is_err()
        );
    }
    assert!(
        dispatch
            .graph("graph", NonZeroU64::MIN.saturating_add(1))
            .await
            .is_err()
    );
    database
        .runs()
        .close_session("session")
        .await
        .expect("close admission");
    assert!(
        dispatch
            .claim(
                "graph",
                NonZeroU64::MIN,
                "first",
                "closed",
                Path::new("workspace")
            )
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_run")
        .fetch_one(database.pool())
        .await
        .expect("count");
    assert_eq!(count, 0);
    let missing = GraphExecutionRecord {
        execution: GraphExecutionRef::UtilityRun {
            run_id: "missing".into(),
        },
        graph_id: "graph".into(),
        graph_revision: NonZeroU64::MIN,
        inputs: Vec::new(),
        node_id: "first".into(),
    };
    assert!(dispatch.fail(&missing, "missing").await.is_err());
}

#[tokio::test]
async fn validation_gate_needs_no_input_and_produced_or_failed_results_do_not_release_it() {
    // Arrange
    let database = crate::test_support::task_database_fixture().await;
    let mut graph = graph();
    graph.nodes[0].execution.context = GraphContext::TaskConversation {
        task_id: "task".into(),
    };
    graph.nodes[0].execution.response_profile = ag_protocol::ProtocolRequestProfile::SessionTurn;
    graph.nodes[0].outputs[0].schema = ag_protocol::ProtocolRequestProfile::SessionTurn;
    graph.nodes[1].inputs.clear();
    graph.nodes[1].dependencies[0].gate = GraphCompletionGate::ResultValidated {
        output_name: "response".into(),
    };
    database
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("graph");
    let first = dispatched(claim(&database, "first", "source").await);

    // Act
    finish(
        &database,
        &first,
        r#"{"answer":"Need input","questions":[{"text":"Which target?"}]}"#,
    )
    .await;

    // Assert
    assert_eq!(
        claim(&database, "second", "not-validated").await,
        GraphAdmission::Blocked
    );
    let results = database
        .graphs()
        .list_results("graph", "first", "response")
        .await
        .expect("results");
    assert!(
        database
            .graphs()
            .validate_result(
                &results[0].result.reference,
                &GraphEvidence {
                    actor: "host".into(),
                    reason: "cannot override questions".into()
                }
            )
            .await
            .is_err()
    );
    assert!(results[0].validation.is_none());
}
