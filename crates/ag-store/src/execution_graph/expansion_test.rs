//! Append-only expansion tests: revision checks, idempotency, ledger rules,
//! and work addressed to revisions that only expansions followed.

use std::num::NonZeroU64;
use std::path::Path;

use ag_session::{
    ContinueGraphNodeRequest, ExecutionGraph, ExecutionGraphStatus, ExpandExecutionGraphRequest,
    GraphCompletionGate, GraphControlRequest, GraphDependency, GraphExecutionRef,
    GraphInputBinding, GraphNode, GraphNodeContinuation, GraphNodeStatus,
};
use ag_worker::{GraphAdmission, GraphDispatch, RunState};

use crate::Database;
use crate::execution_graph::support_test::{evidence, graph, reader};

#[tokio::test]
async fn expanded_nodes_dispatch_with_inputs_pinned_under_the_earlier_revision() {
    // Arrange
    let database = database(&graph()).await;
    let first = dispatched(claim(&database, NonZeroU64::MIN, "first", "first-run").await);
    finish(&database, &first, r#"{"answer":"exact evidence"}"#).await;
    let request = expansion("expansion", NonZeroU64::MIN, vec![consumer("third")]);

    // Act
    let recorded = database
        .graphs()
        .expand_graph(&request)
        .await
        .expect("expand");
    let redelivered = database
        .graphs()
        .expand_graph(&request)
        .await
        .expect("redeliver");
    let third = dispatched(claim(&database, NonZeroU64::MIN, "third", "third-run").await);
    let accepted = database
        .graphs()
        .accept_result(&third.inputs[0].reference.result, &evidence())
        .await
        .expect("accept");
    let second = dispatched(claim(&database, NonZeroU64::MIN, "second", "second-run").await);
    let progress = database
        .graphs()
        .progress("graph")
        .await
        .expect("progress")
        .expect("graph");

    // Assert
    assert!(recorded);
    assert!(!redelivered);
    let current = database
        .graphs()
        .load_graph("graph", None)
        .await
        .expect("load")
        .expect("graph");
    assert_eq!(current.revision.get(), 2);
    assert_eq!(current.nodes[..2], graph().nodes[..]);
    assert_eq!(third.record.graph_revision.get(), 2);
    assert_eq!(
        third.inputs[0].reference.result.graph_revision,
        NonZeroU64::MIN
    );
    assert_eq!(third.inputs[0].body, r#"{"answer":"exact evidence"}"#);
    assert!(accepted);
    assert_eq!(
        second.inputs[0].reference.result.graph_revision,
        NonZeroU64::MIN
    );
    assert_eq!(progress.revision.get(), 2);
    assert_eq!(
        progress
            .nodes
            .iter()
            .map(|node| (node.node_id.as_str(), node.status, node.executions.len()))
            .collect::<Vec<_>>(),
        [
            ("first", GraphNodeStatus::Succeeded, 1),
            ("second", GraphNodeStatus::Queued, 1),
            ("third", GraphNodeStatus::Queued, 1),
        ]
    );
    assert_eq!(
        database
            .graphs()
            .save_graph(&next_revision(&current), Some(current.revision))
            .await
            .expect_err("linked graphs only expand")
            .to_string(),
        "Invalid execution graph: Cannot revise a graph with execution links"
    );
}

#[tokio::test]
async fn invalid_or_conflicting_expansions_record_nothing() {
    // Arrange
    let database = database(&graph()).await;
    database
        .graphs()
        .expand_graph(&expansion("taken", NonZeroU64::MIN, vec![reader("third")]))
        .await
        .expect("first expansion");
    let current = NonZeroU64::new(2).expect("revision");
    let mut unbudgeted = reader("fourth");
    unbudgeted.execution.shared_budget_id = None;
    let mut foreign = reader("fourth");
    foreign.execution.shared_budget_id = Some("other".into());
    let mut dangling = reader("fourth");
    dangling.dependencies.push(GraphDependency {
        gate: GraphCompletionGate::ExecutionSucceeded,
        node_id: "missing".into(),
    });

    // Act
    let rejected = [
        expansion(" ", current, vec![reader("fourth")]),
        expansion("taken", current, vec![reader("fourth")]),
        expansion("stale", NonZeroU64::MIN, vec![reader("fourth")]),
        expansion(
            "ahead",
            NonZeroU64::new(3).expect("revision"),
            vec![reader("fourth")],
        ),
        expansion("missing", current, vec![reader("fourth")]),
        expansion("reused", current, vec![reader("first")]),
        expansion("empty", current, Vec::new()),
        expansion("dangling", current, vec![dangling]),
        expansion("unbudgeted", current, vec![unbudgeted]),
        expansion("foreign", current, vec![foreign]),
    ];
    let mut messages = Vec::new();
    for (index, mut request) in rejected.into_iter().enumerate() {
        if index == 4 {
            request.graph.graph_id = "absent".into();
        }
        messages.push(
            database
                .graphs()
                .expand_graph(&request)
                .await
                .expect_err("rejected expansion")
                .to_string(),
        );
    }

    // Assert
    for (message, expected) in messages.iter().zip([
        "requires an identity",
        "used for a different expansion",
        "must extend the current revision",
        "must extend the current revision",
        "Graph does not exist",
        "duplicate graph node: first",
        "must append at least one node",
        "unknown graph node: missing",
        "existing provider budgets",
        "existing provider budgets",
    ]) {
        assert!(message.contains(expected), "{message}");
    }
    let stored = database
        .graphs()
        .load_graph("graph", None)
        .await
        .expect("load")
        .expect("graph");
    assert_eq!(stored.revision, current);
}

#[tokio::test]
async fn canceled_and_completed_graphs_cannot_be_expanded() {
    // Arrange
    let mut single = graph();
    single.nodes.truncate(1);
    let completed = database(&single).await;
    let first = dispatched(claim(&completed, NonZeroU64::MIN, "first", "first-run").await);
    finish(&completed, &first, r#"{"answer":"done"}"#).await;
    let canceled = database(&graph()).await;
    canceled
        .graphs()
        .cancel(&GraphControlRequest {
            graph_id: "graph".into(),
            revision: NonZeroU64::MIN,
        })
        .await
        .expect("cancel");
    let request = expansion("expansion", NonZeroU64::MIN, vec![reader("third")]);

    // Act
    let after_completion = completed.graphs().expand_graph(&request).await;
    let after_cancellation = canceled.graphs().expand_graph(&request).await;

    // Assert
    assert_eq!(
        completed
            .graphs()
            .progress("graph")
            .await
            .expect("progress")
            .expect("graph")
            .status,
        ExecutionGraphStatus::Completed
    );
    assert!(
        after_completion
            .expect_err("completed")
            .to_string()
            .contains("Completed graphs cannot be expanded")
    );
    assert!(
        after_cancellation
            .expect_err("canceled")
            .to_string()
            .contains("Canceled graphs cannot be expanded")
    );
}

#[tokio::test]
async fn a_full_revision_after_an_expansion_retires_the_earlier_revisions() {
    // Arrange
    let database = database(&graph()).await;
    database
        .graphs()
        .expand_graph(&expansion(
            "expansion",
            NonZeroU64::MIN,
            vec![reader("third")],
        ))
        .await
        .expect("expansion");
    let expanded = database
        .graphs()
        .load_graph("graph", None)
        .await
        .expect("load")
        .expect("graph");
    let rewritten = next_revision(&expanded);
    database
        .graphs()
        .save_graph(&rewritten, Some(expanded.revision))
        .await
        .expect("unlinked graphs still accept full revisions");

    // Act
    let dispatch = database.graph_dispatch();
    let mut errors = Vec::new();
    for revision in [NonZeroU64::MIN, expanded.revision] {
        errors.push(
            dispatch
                .graph("graph", revision)
                .await
                .expect_err("retired revision")
                .to_string(),
        );
    }
    let ahead = database
        .graph_dispatch()
        .graph("graph", NonZeroU64::new(4).expect("revision"))
        .await;
    let current = database
        .graph_dispatch()
        .graph("graph", rewritten.revision)
        .await;

    // Assert
    for error in errors {
        assert!(error.contains("Stale graph revision"), "{error}");
    }
    assert!(ahead.is_err());
    assert_eq!(current.expect("current"), rewritten);
}

#[tokio::test]
async fn retries_continue_across_an_expansion_and_keep_the_node_history() {
    // Arrange
    let database = database(&graph()).await;
    let first = dispatched(claim(&database, NonZeroU64::MIN, "first", "first-run").await);
    database
        .runs()
        .transition(run_id(&first), RunState::Failed, Some("provider failed"))
        .await
        .expect("fail run");
    database
        .graph_dispatch()
        .fail(&first.record, "provider failed")
        .await
        .expect("fail");
    let retry_id = uuid::Uuid::new_v4().to_string();
    let retry = ContinueGraphNodeRequest {
        action_id: retry_id.clone(),
        execution: first.record.execution.clone(),
        graph_id: "graph".into(),
        kind: GraphNodeContinuation::Retry,
        node_id: "first".into(),
        revision: NonZeroU64::MIN,
    };
    database
        .graphs()
        .expand_graph(&expansion(
            "expansion",
            NonZeroU64::MIN,
            vec![reader("third")],
        ))
        .await
        .expect("expansion");
    let mut current_retry = retry.clone();
    current_retry.action_id = uuid::Uuid::new_v4().to_string();
    current_retry.revision = NonZeroU64::new(2).expect("revision");

    // Act
    let recorded = database
        .graphs()
        .continue_node(&retry)
        .await
        .expect("retry under the earlier revision");
    let duplicate = database
        .graphs()
        .continue_node(&current_retry)
        .await
        .expect_err("source already has a pending continuation")
        .to_string();
    let retried = dispatched(claim(&database, NonZeroU64::MIN, "first", "ignored").await);
    let history = database
        .graphs()
        .execution_history(&retried.record.execution)
        .await
        .expect("history")
        .expect("retried execution");
    database
        .runs()
        .transition(run_id(&retried), RunState::Failed, Some("provider failed"))
        .await
        .expect("fail retried run");
    database
        .graph_dispatch()
        .fail(&retried.record, "provider failed")
        .await
        .expect("fail retried");
    current_retry.action_id = uuid::Uuid::new_v4().to_string();
    current_retry.execution = retried.record.execution.clone();
    let recorded_current = database
        .graphs()
        .continue_node(&current_retry)
        .await
        .expect("retry under the expanded revision");

    // Assert
    assert!(recorded);
    assert!(duplicate.contains("superseded by explicit continuation intent"));
    assert!(recorded_current);
    assert_eq!(
        retried.record.execution,
        GraphExecutionRef::UtilityRun { run_id: retry_id }
    );
    assert_eq!(retried.record.graph_revision.get(), 2);
    assert!(history.continuation_prompt.is_some());
}

async fn database(graph: &ExecutionGraph) -> Database {
    let database = crate::test_support::task_database_fixture().await;
    database
        .graphs()
        .save_graph(graph, None)
        .await
        .expect("graph");

    database
}

async fn claim(
    database: &Database,
    revision: NonZeroU64,
    node: &str,
    execution_id: &str,
) -> GraphAdmission {
    database
        .graph_dispatch()
        .claim(
            "graph",
            revision,
            node,
            execution_id,
            Path::new("workspace"),
        )
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

/// The utility run that `dispatch` admitted.
fn run_id(dispatch: &GraphDispatch) -> &str {
    let run_id = match &dispatch.record.execution {
        GraphExecutionRef::UtilityRun { run_id } => Some(run_id.as_str()),
        GraphExecutionRef::TaskAttempt { .. } => None,
    };

    run_id.expect("expected a utility run")
}

async fn finish(database: &Database, dispatch: &GraphDispatch, body: &str) {
    database
        .runs()
        .transition(run_id(dispatch), RunState::Completed, None)
        .await
        .expect("complete");
    database
        .graph_dispatch()
        .finish(dispatch, body)
        .await
        .expect("response");
}

/// A reader that consumes the validated `response` of `first`.
fn consumer(id: &str) -> GraphNode {
    let mut node = reader(id);
    node.dependencies.push(GraphDependency {
        gate: GraphCompletionGate::ResultValidated {
            output_name: "response".into(),
        },
        node_id: "first".into(),
    });
    node.inputs.push(GraphInputBinding {
        name: "evidence".into(),
        output_name: "response".into(),
        producer_node_id: "first".into(),
    });

    node
}

fn expansion(id: &str, revision: NonZeroU64, nodes: Vec<GraphNode>) -> ExpandExecutionGraphRequest {
    ExpandExecutionGraphRequest {
        graph: GraphControlRequest {
            graph_id: "graph".into(),
            revision,
        },
        id: id.into(),
        nodes,
    }
}

/// The next revision of `graph` with its first node's objective rewritten.
fn next_revision(graph: &ExecutionGraph) -> ExecutionGraph {
    let mut next = graph.clone();
    next.nodes[0].objective = "Rewritten".into();
    next.revision = graph.revision.checked_add(1).expect("revision");

    next
}
