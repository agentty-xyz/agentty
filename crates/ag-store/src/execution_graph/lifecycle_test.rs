use std::num::NonZeroU64;
use std::path::Path;

use ag_session::{
    ContinueGraphNodeRequest, ExecutionGraphStatus, GraphBlockedReason, GraphContext,
    GraphControlRequest, GraphNodeContinuation, GraphNodeStatus, GraphQuestionAnswer,
};
use ag_worker::{GraphAdmission, GraphDispatch, RunState};

use crate::Database;
use crate::execution_graph::support_test::{evidence, graph, produced, reader, record};

type ContinuationMutation = fn(&mut ContinueGraphNodeRequest);

async fn database(task: bool) -> Database {
    let database = crate::test_support::task_database_fixture().await;
    let mut snapshot = graph();
    if task {
        snapshot.nodes[0].execution.context = GraphContext::TaskConversation {
            task_id: "task".into(),
        };
        snapshot.nodes[0].execution.response_profile =
            ag_protocol::ProtocolRequestProfile::SessionTurn;
        snapshot.nodes[0].outputs[0].schema = ag_protocol::ProtocolRequestProfile::SessionTurn;
    }
    database
        .graphs()
        .save_graph(&snapshot, None)
        .await
        .expect("graph");

    database
}

async fn claim(database: &Database, node: &str) -> GraphDispatch {
    let admission = database
        .graph_dispatch()
        .claim(
            "graph",
            NonZeroU64::MIN,
            node,
            &uuid::Uuid::new_v4().to_string(),
            Path::new("workspace"),
        )
        .await
        .expect("admit");
    let dispatch = match admission {
        GraphAdmission::Claimed(dispatch) => Some(dispatch),
        _ => None,
    }
    .expect("expected new claim");

    *dispatch
}

fn action(dispatch: &GraphDispatch, kind: GraphNodeContinuation) -> ContinueGraphNodeRequest {
    ContinueGraphNodeRequest {
        action_id: uuid::Uuid::new_v4().to_string(),
        execution: dispatch.record.execution.clone(),
        graph_id: dispatch.record.graph_id.clone(),
        kind,
        node_id: dispatch.record.node_id.clone(),
        revision: dispatch.record.graph_revision,
    }
}

async fn settle(database: &Database, dispatch: &GraphDispatch, body: &str) {
    match &dispatch.record.execution {
        ag_session::GraphExecutionRef::UtilityRun { run_id } => {
            database
                .runs()
                .transition(run_id, RunState::Completed, None)
                .await
                .expect("settle utility");
        }
        ag_session::GraphExecutionRef::TaskAttempt { operation_id, .. } => {
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
    database
        .graph_dispatch()
        .finish(dispatch, body)
        .await
        .expect("response");
}

async fn clarification_fixture() -> (Database, GraphDispatch, ContinueGraphNodeRequest) {
    // Arrange
    let database = database(true).await;
    let repository = database.graphs();
    assert!(
        repository
            .progress("missing")
            .await
            .expect("missing")
            .is_none()
    );
    assert!(
        repository
            .execution_history(&ag_session::GraphExecutionRef::UtilityRun {
                run_id: "missing".into()
            })
            .await
            .expect("missing")
            .is_none()
    );
    let initial = repository
        .progress("graph")
        .await
        .expect("progress")
        .expect("graph");
    assert_eq!(initial.status, ExecutionGraphStatus::Running);
    assert!(matches!(
        initial.nodes[1].blocked[0],
        GraphBlockedReason::Dependency { .. }
    ));
    let first = claim(&database, "first").await;
    assert!(
        repository
            .execution_history(&first.record.execution)
            .await
            .expect("initial history")
            .is_none()
    );
    assert_eq!(
        repository
            .progress("graph")
            .await
            .expect("queued")
            .expect("graph")
            .nodes[0]
            .status,
        GraphNodeStatus::Queued
    );
    let operation_id = match &first.record.execution {
        ag_session::GraphExecutionRef::TaskAttempt { operation_id, .. } => Some(operation_id),
        ag_session::GraphExecutionRef::UtilityRun { .. } => None,
    }
    .expect("task");
    database
        .operations()
        .mark_session_operation_running(operation_id)
        .await
        .expect("running");
    assert_eq!(
        repository
            .progress("graph")
            .await
            .expect("running")
            .expect("graph")
            .nodes[0]
            .status,
        GraphNodeStatus::Running
    );
    database
        .operations()
        .mark_session_operation_done(operation_id)
        .await
        .expect("done");
    database
        .graph_dispatch()
        .finish(
            &first,
            r#"{"answer":"Need input","questions":[{"text":"Which target?","options":["Local"]}]}"#,
        )
        .await
        .expect("question response");
    let progress = repository
        .progress("graph")
        .await
        .expect("progress")
        .expect("graph");
    assert_eq!(progress.status, ExecutionGraphStatus::Blocked);
    assert_eq!(progress.nodes[0].status, GraphNodeStatus::AwaitingAnswers);
    assert_eq!(progress.nodes[0].questions[0].text, "Which target?");
    let request = action(
        &first,
        GraphNodeContinuation::Answer {
            answers: vec![GraphQuestionAnswer {
                question: "Which target?".into(),
                answer: "Local".into(),
            }],
        },
    );

    (database, first, request)
}

async fn assert_invalid_replies(
    database: &Database,
    source: &GraphDispatch,
    request: &ContinueGraphNodeRequest,
) {
    let repository = database.graphs();

    // Act / Assert: partial, reordered, blank and stale replies never authorize
    // work.
    for answers in [
        vec![],
        vec![GraphQuestionAnswer {
            question: "Other".into(),
            answer: "Local".into(),
        }],
        vec![GraphQuestionAnswer {
            question: "Which target?".into(),
            answer: " ".into(),
        }],
    ] {
        let mut invalid = request.clone();
        invalid.kind = GraphNodeContinuation::Answer { answers };
        assert!(repository.continue_node(&invalid).await.is_err());
    }
    assert!(
        repository
            .continue_node(&action(source, GraphNodeContinuation::Retry))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn graph_lifecycle_clarification_is_exact_and_new_attempts_retain_history() {
    // Arrange
    let (database, first, mut request) = clarification_fixture().await;
    let repository = database.graphs();
    assert_invalid_replies(&database, &first, &request).await;

    // Act / Assert
    assert!(repository.continue_node(&request).await.expect("reply"));
    assert!(
        !repository
            .continue_node(&request)
            .await
            .expect("identical reply")
    );
    assert!(
        database
            .tasks()
            .claim_attempt(
                "session",
                &ag_session::SubmitTaskRequest {
                    attempt_id: request.action_id.clone(),
                    message: serde_json::json!(first.node.request).to_string(),
                    objective: first.node.objective.clone(),
                    task_id: "task".into(),
                    visibility: ag_session::DurableMessageVisibility::Hidden,
                },
                "graph_node"
            )
            .await
            .is_err(),
        "legacy submission cannot consume graph continuation intent"
    );
    let mut conflict = request.clone();
    conflict.kind = GraphNodeContinuation::Retry;
    assert!(repository.continue_node(&conflict).await.is_err());
    assert_eq!(
        repository
            .progress("graph")
            .await
            .expect("pending")
            .expect("graph")
            .nodes[0]
            .status,
        GraphNodeStatus::Pending
    );
    let second = claim(&database, "first").await;
    let history = repository
        .execution_history(&second.record.execution)
        .await
        .expect("history")
        .expect("isolated replay")
        .text;
    assert!(history.contains("Need input"));
    assert!(history.contains("Local"));
    assert_ne!(second.record.execution, first.record.execution);
    request.action_id = uuid::Uuid::new_v4().to_string();
    assert!(repository.continue_node(&request).await.is_err());
    assert!(
        database
            .graph_dispatch()
            .finish(&first, r#"{"answer":"stale"}"#)
            .await
            .is_err()
    );
    settle(&database, &second, r#"{"answer":"ready"}"#).await;
    assert_clarification_handoff(&database).await;
}

async fn assert_clarification_handoff(database: &Database) {
    let repository = database.graphs();
    let progress = repository
        .progress("graph")
        .await
        .expect("progress")
        .expect("graph");
    assert_eq!(progress.nodes[0].executions.len(), 2);
    assert_eq!(progress.nodes[0].status, GraphNodeStatus::Succeeded);
    assert_eq!(progress.nodes[0].results.len(), 1);
    let reference = &progress.nodes[0].results[0].reference;
    assert_eq!(reference.version.get(), 2);
    repository
        .accept_result(reference, &evidence())
        .await
        .expect("accept exact continuation");
    let consumer = claim(database, "second").await;
    assert_eq!(consumer.inputs[0].reference.result, *reference);
    settle(database, &consumer, r#"{"answer":"consumed"}"#).await;
    assert_eq!(
        repository
            .progress("graph")
            .await
            .expect("complete")
            .expect("graph")
            .status,
        ExecutionGraphStatus::Completed
    );
    assert!(
        database
            .tasks()
            .load_task("task")
            .await
            .expect("task")
            .expect("task")
            .completed_at
            .is_none()
    );
}

#[tokio::test]
async fn graph_lifecycle_retry_is_idempotent_and_keeps_the_source_inputs() {
    // Arrange
    let database = database(false).await;
    let repository = database.graphs();
    let producer = claim(&database, "first").await;
    settle(&database, &producer, r#"{"answer":"pinned"}"#).await;
    let reference = repository
        .progress("graph")
        .await
        .expect("progress")
        .expect("graph")
        .nodes[0]
        .results[0]
        .reference
        .clone();
    repository
        .accept_result(&reference, &evidence())
        .await
        .expect("accept");
    let source = claim(&database, "second").await;
    database
        .graph_dispatch()
        .fail(&source.record, "provider failed")
        .await
        .expect("failure");
    let failed = repository
        .progress("graph")
        .await
        .expect("failed")
        .expect("graph");
    assert_eq!(failed.status, ExecutionGraphStatus::Failed);
    assert_eq!(
        failed.nodes[1].last_error.as_deref(),
        Some("provider failed")
    );
    let request = action(&source, GraphNodeContinuation::Retry);

    // Act
    assert!(repository.continue_node(&request).await.expect("authorize"));
    let dispatch_repository = database.graph_dispatch();
    let (first, second) = tokio::join!(
        dispatch_repository.claim(
            "graph",
            NonZeroU64::MIN,
            "second",
            "ignored-a",
            Path::new("workspace")
        ),
        dispatch_repository.claim(
            "graph",
            NonZeroU64::MIN,
            "second",
            "ignored-b",
            Path::new("workspace")
        ),
    );
    let retry = match (first.expect("claim"), second.expect("redelivery")) {
        (GraphAdmission::Claimed(dispatch), GraphAdmission::Existing(_))
        | (GraphAdmission::Existing(_), GraphAdmission::Claimed(dispatch)) => Some(*dispatch),
        _ => None,
    }
    .expect("only one continuation may dispatch");

    // Assert
    assert_eq!(retry.record.inputs, source.record.inputs);
    assert_eq!(retry.inputs, source.inputs);
    assert!(
        !repository
            .continue_node(&request)
            .await
            .expect("old delivery")
    );
    assert!(
        matches!(&retry.record.execution, ag_session::GraphExecutionRef::UtilityRun { run_id } if run_id == &request.action_id)
    );
    settle(&database, &retry, r#"{"answer":"retried"}"#).await;
    repository
        .cancel(&GraphControlRequest {
            graph_id: "graph".into(),
            revision: NonZeroU64::MIN,
        })
        .await
        .expect("completed cancellation is a no-op");
    assert_eq!(
        repository
            .progress("graph")
            .await
            .expect("complete")
            .expect("graph")
            .status,
        ExecutionGraphStatus::Completed
    );
}

#[tokio::test]
async fn graph_lifecycle_cancellation_closes_admission_and_rejects_late_actions() {
    // Arrange
    let database = database(true).await;
    let repository = database.graphs();
    let first = claim(&database, "first").await;
    let stale = GraphControlRequest {
        graph_id: "graph".into(),
        revision: NonZeroU64::new(2).expect("revision"),
    };
    assert!(repository.cancel(&stale).await.is_err());
    let control = GraphControlRequest {
        graph_id: "graph".into(),
        revision: NonZeroU64::MIN,
    };

    // Act
    repository.cancel(&control).await.expect("cancel");
    repository
        .cancel(&control)
        .await
        .expect("idempotent cancel");
    let progress = repository
        .progress("graph")
        .await
        .expect("progress")
        .expect("graph");

    // Assert
    assert_eq!(progress.status, ExecutionGraphStatus::Cancelled);
    assert_eq!(progress.nodes[1].status, GraphNodeStatus::Cancelled);
    let operation_id = match &first.record.execution {
        ag_session::GraphExecutionRef::TaskAttempt { operation_id, .. } => Some(operation_id),
        ag_session::GraphExecutionRef::UtilityRun { .. } => None,
    }
    .expect("task");
    assert!(
        database
            .operations()
            .is_cancel_requested_for_operation(operation_id)
            .await
            .expect("cancel flag")
    );
    assert!(
        repository
            .continue_node(&action(&first, GraphNodeContinuation::Retry))
            .await
            .is_err()
    );
    assert!(matches!(
        database
            .graph_dispatch()
            .claim(
                "graph",
                NonZeroU64::MIN,
                "second",
                "no-work",
                Path::new("workspace")
            )
            .await
            .expect("closed admission"),
        GraphAdmission::Blocked
    ));
    assert!(
        database
            .graph_dispatch()
            .finish(&first, r#"{"answer":"late"}"#)
            .await
            .is_err()
    );
    database
        .operations()
        .mark_session_operation_canceled(operation_id, "canceled")
        .await
        .expect("settled cleanup");
    assert_eq!(
        repository
            .progress("graph")
            .await
            .expect("settled")
            .expect("graph")
            .nodes[0]
            .status,
        GraphNodeStatus::Cancelled
    );
}

#[tokio::test]
async fn graph_lifecycle_continuation_rejects_invalid_identity_ownership_and_execution_state() {
    // Arrange
    let database = database(false).await;
    let repository = database.graphs();
    let source = claim(&database, "first").await;
    let request = action(&source, GraphNodeContinuation::Retry);
    assert!(
        repository.continue_node(&request).await.is_err(),
        "queued source"
    );
    database
        .graph_dispatch()
        .fail(&source.record, "failed")
        .await
        .expect("fail");
    let mut other = graph();
    other.id = "other".into();
    repository
        .save_graph(&other, None)
        .await
        .expect("other graph");

    // Act / Assert
    let cases: &[ContinuationMutation] = &[
        |request| request.action_id = "not-a-uuid".into(),
        |request| {
            request.execution = ag_session::GraphExecutionRef::UtilityRun {
                run_id: "missing".into(),
            }
        },
        |request| request.node_id = "second".into(),
        |request| request.graph_id = "other".into(),
        |request| request.revision = NonZeroU64::new(2).expect("revision"),
        |request| request.kind = GraphNodeContinuation::Answer { answers: vec![] },
    ];
    for mutate in cases {
        let mut invalid = request.clone();
        mutate(&mut invalid);
        assert!(repository.continue_node(&invalid).await.is_err());
    }
    let mut reused = request.clone();
    let run_id = match &source.record.execution {
        ag_session::GraphExecutionRef::UtilityRun { run_id } => Some(run_id),
        ag_session::GraphExecutionRef::TaskAttempt { .. } => None,
    }
    .expect("utility source");
    reused.action_id.clone_from(run_id);
    assert!(repository.continue_node(&reused).await.is_err());
    database
        .graphs()
        .continue_node(&request)
        .await
        .expect("valid retry");
    let competing = action(&source, GraphNodeContinuation::Retry);
    assert!(repository.continue_node(&competing).await.is_err());
}

#[tokio::test]
async fn graph_lifecycle_incomplete_settlement_retry_fences_late_response_and_acceptance() {
    // Arrange
    let database = database(false).await;
    let source = claim(&database, "first").await;
    let run_id = match &source.record.execution {
        ag_session::GraphExecutionRef::UtilityRun { run_id } => Some(run_id),
        ag_session::GraphExecutionRef::TaskAttempt { .. } => None,
    }
    .expect("utility source");
    database
        .runs()
        .transition(run_id, RunState::Completed, None)
        .await
        .expect("authority settled before graph publication");
    let progress = database
        .graphs()
        .progress("graph")
        .await
        .expect("progress")
        .expect("graph");
    assert_eq!(progress.nodes[0].status, GraphNodeStatus::Failed);
    assert!(
        progress.nodes[0]
            .last_error
            .as_ref()
            .expect("gap reason")
            .contains("without a graph response")
    );
    let retry = action(&source, GraphNodeContinuation::Retry);

    // Act
    database
        .graphs()
        .continue_node(&retry)
        .await
        .expect("explicit gap recovery");
    assert!(
        database
            .graph_dispatch()
            .finish(&source, r#"{"answer":"late source response"}"#)
            .await
            .is_err()
    );
    let next = claim(&database, "first").await;
    settle(&database, &next, r#"{"answer":"new response"}"#).await;
    let result = database
        .graphs()
        .progress("graph")
        .await
        .expect("progress")
        .expect("graph")
        .nodes[0]
        .results[0]
        .reference
        .clone();
    database
        .graphs()
        .cancel(&GraphControlRequest {
            graph_id: "graph".into(),
            revision: NonZeroU64::MIN,
        })
        .await
        .expect("cancel before acceptance");

    // Assert
    assert!(
        database
            .graphs()
            .accept_result(&result, &evidence())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn graph_lifecycle_progress_reports_every_dependency_gate_and_conflicting_env_work() {
    // Arrange
    let database = crate::test_support::task_database_fixture().await;
    let mut snapshot = graph();
    for (id, gate) in [
        (
            "execution",
            ag_session::GraphCompletionGate::ExecutionSucceeded,
        ),
        (
            "validation",
            ag_session::GraphCompletionGate::ResultValidated {
                output_name: "response".into(),
            },
        ),
    ] {
        let mut node = reader(id);
        node.dependencies.push(ag_session::GraphDependency {
            gate,
            node_id: "first".into(),
        });
        snapshot.nodes.push(node);
    }
    database
        .graphs()
        .save_graph(&snapshot, None)
        .await
        .expect("graph");
    database
        .operations()
        .insert_session_operation("ordinary", "session", "chat")
        .await
        .expect("host work");

    // Act / Assert
    let progress = database
        .graphs()
        .progress("graph")
        .await
        .expect("progress")
        .expect("graph");
    assert!(
        progress.nodes[0]
            .blocked
            .contains(&GraphBlockedReason::EnvironmentBusy)
    );
    assert_eq!(progress.status, ExecutionGraphStatus::Blocked);
    database
        .operations()
        .mark_session_operation_done("ordinary")
        .await
        .expect("host settled");
    let source = claim(&database, "first").await;
    let blocked = database
        .graphs()
        .progress("graph")
        .await
        .expect("blocked")
        .expect("graph");
    assert!(
        blocked.nodes[2]
            .blocked
            .iter()
            .any(|reason| matches!(reason, GraphBlockedReason::Dependency { .. }))
    );
    assert!(
        blocked.nodes[3]
            .blocked
            .iter()
            .any(|reason| matches!(reason, GraphBlockedReason::Dependency { .. }))
    );
    settle(&database, &source, r#"{"answer":"ready"}"#).await;
    let progress = database
        .graphs()
        .progress("graph")
        .await
        .expect("ready")
        .expect("graph");
    assert_eq!(
        progress.nodes[2].blocked,
        [GraphBlockedReason::AwaitingDispatch]
    );
    assert_eq!(
        progress.nodes[3].blocked,
        [GraphBlockedReason::AwaitingDispatch]
    );
    assert!(matches!(
        progress.nodes[1].blocked[0],
        GraphBlockedReason::Dependency { .. }
    ));
}

#[tokio::test]
async fn graph_lifecycle_retry_retains_observed_task_turns_when_ordered_effects_failed() {
    // Arrange
    let database = database(true).await;
    let source = claim(&database, "first").await;
    let operation_id = match &source.record.execution {
        ag_session::GraphExecutionRef::TaskAttempt { operation_id, .. } => Some(operation_id),
        ag_session::GraphExecutionRef::UtilityRun { .. } => None,
    }
    .expect("task");
    database
        .operations()
        .mark_session_operation_running(operation_id)
        .await
        .expect("running");
    let metadata = crate::TaskAgentTurnMetadata {
        assistant_message: r#"{"answer":"Observed changes before effects failed"}"#.into(),
        harness_turn_report: None,
        instruction_conversation_id: None,
        input_tokens: 1,
        output_tokens: 2,
        provider_conversation_id: None,
    };
    database
        .graphs()
        .record_task_turn(&source, &metadata)
        .await
        .expect("observed response");
    database
        .graph_dispatch()
        .fail(&source.record, "effects failed")
        .await
        .expect("settle effects failure");
    let answer = action(&source, GraphNodeContinuation::Answer { answers: vec![] });
    assert!(database.graphs().continue_node(&answer).await.is_err());

    // Act
    database
        .graphs()
        .continue_node(&action(&source, GraphNodeContinuation::Retry))
        .await
        .expect("retry");
    let retry = claim(&database, "first").await;
    let history = database
        .graphs()
        .execution_history(&retry.record.execution)
        .await
        .expect("history")
        .expect("retained failed observation")
        .text;

    // Assert
    assert!(history.contains("Observed changes before effects failed"));
    assert!(
        database
            .graphs()
            .record_task_turn(&source, &metadata)
            .await
            .is_err()
    );
    sqlx::query("UPDATE execution_graph_task_turn SET metadata = 'invalid'")
        .execute(database.pool())
        .await
        .expect("corrupt metadata");
    assert!(
        database
            .graphs()
            .execution_history(&retry.record.execution)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn graph_lifecycle_legacy_links_cannot_authorize_new_dispatch() {
    // Arrange
    let database = produced().await;
    let linked = GraphDispatch {
        inputs: Vec::new(),
        node: graph().nodes.remove(0),
        record: record("run"),
    };

    // Act / Assert
    assert!(
        database
            .graphs()
            .continue_node(&action(&linked, GraphNodeContinuation::Retry))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn graph_lifecycle_clarification_rejects_missing_nodes_and_response_evidence() {
    for scenario in ["missing-node", "missing-response", "malformed-response"] {
        // Arrange
        let (database, _, mut request) = clarification_fixture().await;
        match scenario {
            "missing-node" => {
                request.node_id = "missing".into();
                sqlx::query("UPDATE execution_graph_execution SET node_id = 'missing'")
                    .execute(database.pool())
                    .await
                    .expect("corrupt node identity");
            }
            "missing-response" => {
                sqlx::query("DELETE FROM execution_graph_response")
                    .execute(database.pool())
                    .await
                    .expect("remove response evidence");
            }
            _ => {
                sqlx::query("UPDATE execution_graph_response SET body = 'invalid response'")
                    .execute(database.pool())
                    .await
                    .expect("corrupt response evidence");
            }
        }

        // Act
        let result = database.graphs().continue_node(&request).await;

        // Assert
        let error = result
            .expect_err("invalid continuation evidence")
            .to_string();
        if scenario == "missing-node" {
            assert!(error.contains("Continuation node is missing"));
        } else if scenario == "missing-response" {
            assert!(error.contains("Clarification response is missing"));
        } else {
            assert!(error.contains("execution graph"));
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_graph_continuation")
            .fetch_one(database.pool())
            .await
            .expect("continuation count");
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn graph_lifecycle_retry_history_omits_unobserved_failed_turns() {
    // Arrange
    let database = database(true).await;
    let source = claim(&database, "first").await;
    database
        .graph_dispatch()
        .fail(&source.record, "provider failed before a response")
        .await
        .expect("failure");
    database
        .graphs()
        .continue_node(&action(&source, GraphNodeContinuation::Retry))
        .await
        .expect("retry intent");
    let retry = claim(&database, "first").await;

    // Act
    let history = database
        .graphs()
        .execution_history(&retry.record.execution)
        .await
        .expect("history")
        .expect("explicit retry")
        .text;

    // Assert
    assert!(history.contains("Explicitly retry"));
    assert!(!history.contains("Assistant"));
}

#[tokio::test]
async fn graph_lifecycle_retry_rejects_a_missing_pinned_result() {
    // Arrange
    let database = database(false).await;
    let producer = claim(&database, "first").await;
    settle(&database, &producer, r#"{"answer":"pinned"}"#).await;
    let mut reference = database
        .graphs()
        .list_results("graph", "first", "response")
        .await
        .expect("results")
        .remove(0)
        .result
        .reference;
    database
        .graphs()
        .accept_result(&reference, &evidence())
        .await
        .expect("accept");
    let source = claim(&database, "second").await;
    database
        .graph_dispatch()
        .fail(&source.record, "failed")
        .await
        .expect("failure");
    database
        .graphs()
        .continue_node(&action(&source, GraphNodeContinuation::Retry))
        .await
        .expect("retry intent");
    reference.id = "missing-pinned-result".into();
    sqlx::query("UPDATE execution_graph_result SET reference = ?")
        .bind(serde_json::json!(reference).to_string())
        .execute(database.pool())
        .await
        .expect("corrupt pinned identity");

    // Act
    let result = database
        .graph_dispatch()
        .claim(
            "graph",
            NonZeroU64::MIN,
            "second",
            "unused",
            Path::new("workspace"),
        )
        .await;

    // Assert
    assert!(
        result
            .expect_err("missing pinned result")
            .to_string()
            .contains("Pinned input is missing")
    );
    assert_eq!(
        database
            .graphs()
            .list_executions("graph", NonZeroU64::MIN)
            .await
            .expect("retained receipts")
            .len(),
        2
    );
}

#[tokio::test]
async fn graph_lifecycle_retry_rolls_back_when_its_inserted_receipt_disappears() {
    // Arrange
    let database = database(false).await;
    let source = claim(&database, "first").await;
    database
        .graph_dispatch()
        .fail(&source.record, "failed")
        .await
        .expect("failure");
    let request = action(&source, GraphNodeContinuation::Retry);
    database
        .graphs()
        .continue_node(&request)
        .await
        .expect("retry intent");
    sqlx::query(
        "CREATE TRIGGER remove_retry_receipt AFTER INSERT ON execution_graph_execution BEGIN \
         DELETE FROM execution_graph_execution WHERE id = NEW.id; END",
    )
    .execute(database.pool())
    .await
    .expect("inject receipt loss");

    // Act
    let result = database
        .graph_dispatch()
        .claim(
            "graph",
            NonZeroU64::MIN,
            "first",
            "unused",
            Path::new("workspace"),
        )
        .await;

    // Assert
    assert!(
        result
            .expect_err("missing inserted receipt")
            .to_string()
            .contains("Continuation execution is missing")
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_run")
        .fetch_one(database.pool())
        .await
        .expect("run count");
    assert_eq!(count, 1, "the failed admission cannot orphan a utility run");
    let state: (bool, Option<i64>) = sqlx::query_as(
        "SELECT is_admitting, execution_graph_execution_id FROM execution_graph_continuation \
         WHERE id = ?",
    )
    .bind(&request.action_id)
    .fetch_one(database.pool())
    .await
    .expect("retained intent");
    assert_eq!(state, (false, None));
}

#[tokio::test]
async fn graph_history_rejects_a_corrupt_continuation_request() {
    // Arrange
    let (database, _, request) = clarification_fixture().await;
    database
        .graphs()
        .continue_node(&request)
        .await
        .expect("answer");
    let resumed = claim(&database, "first").await;
    sqlx::query("UPDATE execution_graph_continuation SET request = ? WHERE id = ?")
        .bind("{")
        .bind(&request.action_id)
        .execute(database.pool())
        .await
        .expect("inject corrupt request");

    // Act
    let history = database
        .graphs()
        .execution_history(&resumed.record.execution)
        .await;

    // Assert
    assert!(
        history
            .expect_err("invalid persisted request")
            .to_string()
            .contains("EOF")
    );
}
