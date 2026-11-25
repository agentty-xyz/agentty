//! Graph expansion proposals: targeting the graph beside the chat,
//! generation against its captured nodes, and approval that runs the added
//! nodes in the same graph.

use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::AgentRequestKind;
use ag_protocol::{
    GraphProposalDependency, GraphProposalGate, GraphProposalOutcome, GraphProposalResponse,
    TurnPrompt,
};
use ag_session::test_support::{graph_proposal, graph_proposal_state};
use ag_session::{
    ExecutionGraph, ExecutionGraphStatus, GenerateGraphProposalRequest, GraphExecutionRef,
    GraphNodeStatus, GraphProposalState, GraphProposalTarget, GraphProposalVersionOutcome,
    SessionError, StartExecutionGraphRequest,
};
use ag_worker::test_support::{AppServerTurnResponse, MockAppServerClient};
use ag_worker::{GraphAdmission, RunState};

use crate::app::App;
use crate::app::execution_graph::tests::seed_session;
use crate::test_support::{new_test_app, new_test_app_with_clients, test_app_clients};

#[tokio::test]
async fn expansion_proposals_append_nodes_and_run_them_in_the_same_graph() {
    // Arrange
    let clients = test_app_clients().with_app_server_client_override(Arc::new(provider()));
    let (mut app, _directory) = new_test_app_with_clients(clients).await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let service = app.session_service();
    let base_input = graph_proposal_state("base", "sess").input;
    app.drive_session_request(service.generate_graph_proposal(base_input))
        .await
        .expect("generate base");
    let base = proposal(&app, "base").await;
    app.drive_session_request(service.approve_graph_proposal("base", 1))
        .await
        .expect("approve base");
    let graph_id = "proposal:base:1";
    wait_for_node(&app, graph_id, "inspect", GraphNodeStatus::Succeeded).await;

    // Act
    let target = app
        .expandable_graph_target("sess")
        .await
        .expect("target")
        .expect("graph beside the chat");
    let input = GenerateGraphProposalRequest {
        expands: Some(target.clone()),
        id: "expand".into(),
        request: TurnPrompt::from_text("Summarize the findings".into()),
        session_id: "sess".into(),
    };
    app.drive_session_request(service.generate_graph_proposal(input))
        .await
        .expect("generate expansion");
    let expansion = proposal(&app, "expand").await;
    app.drive_session_request(service.approve_graph_proposal("expand", 1))
        .await
        .expect("approve expansion");
    wait_for_node(&app, graph_id, "summary", GraphNodeStatus::Succeeded).await;

    // Assert
    assert_eq!(
        target,
        GraphProposalTarget {
            graph_id: graph_id.into(),
            revision: NonZeroU64::MIN,
        }
    );
    assert_eq!(
        expansion.base.as_ref().map(|base| base.id.as_str()),
        Some(graph_id)
    );
    let launch = launch(&expansion);
    assert!(launch.provider_call_limits.is_empty());
    assert_eq!(launch.graph.revision.get(), 2);
    let stored = app
        .services
        .db()
        .graphs()
        .load_graph(graph_id, None)
        .await
        .expect("load")
        .expect("expanded graph");
    assert_eq!(stored, launch.graph);
    assert_relaunch_matches_the_expanded_revision(&app, &base, &stored).await;
    assert_paused_graph_resumes_for_added_nodes(&mut app, &stored).await;
    app.services.stop_graph_drivers(None);
    app.services.wait_for_cleanup_tasks(None).await;
}

/// Approving nodes for a graph whose driver stopped resumes it, so the added
/// node runs from the credit the graph has left.
async fn assert_paused_graph_resumes_for_added_nodes(app: &mut App, graph: &ExecutionGraph) {
    app.services.stop_graph_drivers(None);
    assert!(!app.has_live_graph_driver(&graph.id));
    let service = app.session_service();
    let input = GenerateGraphProposalRequest {
        expands: Some(GraphProposalTarget {
            graph_id: graph.id.clone(),
            revision: graph.revision,
        }),
        id: "expand-paused".into(),
        request: TurnPrompt::from_text("Recap the findings".into()),
        session_id: "sess".into(),
    };
    app.drive_session_request(service.generate_graph_proposal(input))
        .await
        .expect("generate paused expansion");
    let generated = proposal(app, "expand-paused").await;
    assert_eq!(launch(&generated).graph.revision.get(), 3);
    app.drive_session_request(service.approve_graph_proposal("expand-paused", 1))
        .await
        .expect("approve paused expansion");
    wait_for_node(app, &graph.id, "recap", GraphNodeStatus::Succeeded).await;
    assert!(app.has_live_graph_driver(&graph.id));
}

#[tokio::test]
async fn expansion_requests_need_a_current_graph_that_can_still_run() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let graph = single_node_graph();
    app.services
        .db()
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("graph");
    let no_proposal = app.expandable_graph_target("sess").await;
    let paused = expansion_error(&app, &graph.id, graph.revision).await;
    complete(&app, &graph).await;
    let mut approved = graph_proposal_state("finished", "sess");
    approved.version.approval = Some(ag_session::GraphProposalApproval {
        graph_id: graph.id.clone(),
        limits: approved.limits.clone(),
        version: 1,
    });
    let mut proposals = ag_store::MockGraphProposalRepository::new();
    proposals
        .expect_list()
        .returning(move |_| Ok(vec![approved.clone()]));

    // Act
    let errors = [
        expansion_error(&app, "missing", NonZeroU64::MIN).await,
        expansion_error(&app, &graph.id, NonZeroU64::new(2).expect("revision")).await,
        expansion_error(&app, &graph.id, graph.revision).await,
    ];
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));
    let finished = app.expandable_graph_target("sess").await;

    // Assert
    assert_eq!(no_proposal, Ok(None));
    assert_eq!(paused, None);
    assert_eq!(
        errors,
        [
            Some("Graph to expand is missing".into()),
            Some("The graph changed; request the expansion again".into()),
            Some("Completed or canceled graphs cannot gain nodes".into()),
        ]
    );
    assert_eq!(finished, Ok(None));
}

#[tokio::test]
async fn expansion_requests_skip_later_unapproved_proposals() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let graph = single_node_graph();
    app.services
        .db()
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("graph");
    let mut approved = graph_proposal_state("base", "sess");
    approved.version.approval = Some(ag_session::GraphProposalApproval {
        graph_id: graph.id.clone(),
        limits: approved.limits.clone(),
        version: 1,
    });
    let mut cancelled = graph_proposal_state("expand", "sess");
    cancelled.version.is_cancelled = true;
    let mut proposals = ag_store::MockGraphProposalRepository::new();
    proposals
        .expect_list()
        .returning(move |_| Ok(vec![approved.clone(), cancelled.clone()]));
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));

    // Act
    let target = app.expandable_graph_target("sess").await;

    // Assert
    assert_eq!(
        target,
        Ok(Some(GraphProposalTarget {
            graph_id: graph.id.clone(),
            revision: graph.revision,
        }))
    );
}

/// Reports why an expansion of `graph_id` at `revision` cannot start, or
/// `None` when it can.
async fn expansion_error(app: &App, graph_id: &str, revision: NonZeroU64) -> Option<String> {
    let target = GraphProposalTarget {
        graph_id: graph_id.into(),
        revision,
    };

    match app.expansion_base(&"sess".into(), &target).await {
        Ok(_) => None,
        Err(SessionError::Operation(message)) => Some(message),
        Err(error) => Some(error.to_string()),
    }
}

/// Relaunching after a restart uses the expanded current revision under the
/// limits the original approval fixed; a different revision or limits do
/// not match that approval.
async fn assert_relaunch_matches_the_expanded_revision(
    app: &App,
    base: &GraphProposalState,
    expanded: &ExecutionGraph,
) {
    let approved = launch(base);
    let relaunch = StartExecutionGraphRequest {
        graph: expanded.clone(),
        provider_call_limits: approved.provider_call_limits.clone(),
    };
    let mut other_limits = relaunch.clone();
    other_limits.provider_call_limits.clear();
    let mut unknown_revision = relaunch.clone();
    unknown_revision.graph.nodes[0].objective = "Changed".into();
    let mut earlier = approved.clone();
    earlier.graph.nodes[0].objective = "Changed".into();

    assert!(app.validate_proposal_launch(&approved).await.is_ok());
    assert!(app.validate_proposal_launch(&relaunch).await.is_ok());
    for rejected in [other_limits, unknown_revision, earlier] {
        assert!(app.validate_proposal_launch(&rejected).await.is_err());
    }
}

/// Answers the base and expansion generations, titles, and node turns.
fn provider() -> MockAppServerClient {
    let mut provider = MockAppServerClient::new();
    provider.expect_run_isolated_turn().returning(|request, _| {
        let answer = match request.request_kind {
            AgentRequestKind::GraphProposal => {
                let mut proposal = graph_proposal();
                proposal.nodes[0].agent_id = "planner".into();
                let mut node = proposal.nodes[0].clone();
                if request
                    .prompt
                    .contains("or null for a new graph (JSON data): null")
                {
                    // The base waits at an accepted-output gate, so it can
                    // still gain nodes after its first node settles.
                    node.id = "gated".into();
                    node.dependencies.push(GraphProposalDependency {
                        gate: GraphProposalGate::ResultAccepted {
                            output_name: "report".into(),
                        },
                        node_id: "inspect".into(),
                    });
                    proposal.nodes.push(node);
                } else {
                    assert!(request.prompt.contains(r#""id":"gated""#));
                    node.id = if request.prompt.contains(r#""id":"summary""#) {
                        "recap"
                    } else {
                        "summary"
                    }
                    .into();
                    node.dependencies.push(GraphProposalDependency {
                        gate: GraphProposalGate::ExecutionSucceeded {},
                        node_id: "inspect".into(),
                    });
                    proposal.nodes = vec![node];
                }
                let outcome = GraphProposalOutcome::Proposal { proposal };

                serde_json::json!(GraphProposalResponse { outcome }).to_string()
            }
            _ => r#"{"answer":"Graph node finished"}"#.into(),
        };

        Box::pin(async move {
            Ok(AppServerTurnResponse {
                assistant_message: answer,
                context_reset: false,
                input_tokens: 1,
                output_tokens: 1,
                pid: None,
                provider_conversation_id: None,
            })
        })
    });
    provider
        .expect_shutdown_session()
        .returning(|_| Box::pin(async {}));

    provider
}

/// Waits for proposal `id` to finish generating and returns it.
async fn proposal(app: &App, id: &str) -> GraphProposalState {
    let proposals = app.services.db().graph_proposals();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = proposals
                .get(id, None)
                .await
                .expect("query")
                .expect("proposal");
            if !matches!(
                state.version.outcome,
                GraphProposalVersionOutcome::Generating
            ) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("generation deadline")
}

/// Waits for `node_id` of `graph_id` to reach `status`.
async fn wait_for_node(app: &App, graph_id: &str, node_id: &str, status: GraphNodeStatus) {
    let graphs = app.services.db().graphs();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reached = graphs
                .progress(graph_id)
                .await
                .expect("progress")
                .is_some_and(|progress| {
                    progress
                        .nodes
                        .iter()
                        .any(|node| node.node_id == node_id && node.status == status)
                });
            if reached {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("node deadline");
}

fn launch(state: &GraphProposalState) -> StartExecutionGraphRequest {
    let launch = match &state.version.outcome {
        GraphProposalVersionOutcome::Generated {
            launch: Some(launch),
            ..
        } => Some(launch.as_ref().clone()),
        _ => None,
    };

    launch.expect("expected a resolved proposal")
}

fn single_node_graph() -> ExecutionGraph {
    ExecutionGraph {
        id: "single".into(),
        nodes: vec![ag_session::test_support::graph_node("only", "sess")],
        revision: NonZeroU64::MIN,
        session_id: "sess".into(),
    }
}

/// Runs the single node of `graph` to a settled, question-free response.
async fn complete(app: &App, graph: &ExecutionGraph) {
    let db = app.services.db();
    let admission = db
        .graph_dispatch()
        .claim(
            &graph.id,
            graph.revision,
            "only",
            &uuid::Uuid::new_v4().to_string(),
            Path::new("workspace"),
        )
        .await
        .expect("claim");
    let dispatch = match admission {
        GraphAdmission::Claimed(dispatch) => Some(*dispatch),
        _ => None,
    }
    .expect("expected a claim");
    let run_id = match &dispatch.record.execution {
        GraphExecutionRef::UtilityRun { run_id } => Some(run_id.as_str()),
        GraphExecutionRef::TaskAttempt { .. } => None,
    }
    .expect("expected a utility run");
    db.runs()
        .transition(run_id, RunState::Completed, None)
        .await
        .expect("complete run");
    db.graph_dispatch()
        .finish(&dispatch, r#"{"answer":"done"}"#)
        .await
        .expect("finish");
    let status = app
        .execution_graph_progress(&graph.id)
        .await
        .expect("progress")
        .expect("graph")
        .status;
    assert_eq!(status, ExecutionGraphStatus::Completed);
}
