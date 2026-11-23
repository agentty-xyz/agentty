//! Session API graph tests: the graph summary a session snapshot carries and
//! the read-only graph inspector read, with its selection rules, outputs,
//! and snapshot token.

use std::sync::{Arc, Mutex};

use ag_protocol::{GraphProposalOutcome, GraphProposalResponse, ProtocolRequestProfile};
use ag_session::{
    ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus, GraphNodeStatus,
    GraphProposalApproval, GraphProposalState, GraphProposalVersionOutcome, GraphWorkspaceAccess,
    QuestionItem, SessionError, SessionGraphProposalPhase, SessionGraphRequirement, SessionGraphs,
    SessionGraphsRequest, SessionId, StartExecutionGraphRequest,
};
use ag_store::{DbError, MockGraphProposalRepository, MockGraphRepository, StoredGraphResult};

use super::support::request_session;
use crate::app::App;
use crate::app::execution_graph::tests::seed_session;
use crate::presentation::execution_graph::tests::acceptance_gated_view;
use crate::test_support::new_test_app;

/// Reads the graphs of `sess` through the session API with this selection.
async fn request_graphs(
    app: &mut App,
    graph_id: Option<&str>,
    node_id: Option<&str>,
) -> Result<SessionGraphs, SessionError> {
    let service = app.session_service();
    let request = SessionGraphsRequest {
        graph_id: graph_id.map(str::to_string),
        node_id: node_id.map(str::to_string),
        session_id: SessionId::from("sess"),
    };

    app.drive_session_request(async move { service.session_graphs(request).await })
        .await
}

/// The acceptance-gated graph `graph` of `sess`: `first` succeeded with a
/// validated `response` that `second` waits to see accepted.
fn gated_graph() -> (ExecutionGraph, ExecutionGraphProgress) {
    let view = acceptance_gated_view();

    (
        view.graphs[0].clone(),
        view.progress.expect("gated progress"),
    )
}

/// The latest proposal of `sess`, resolved to `graph` and approved.
fn approved_proposal(graph: &ExecutionGraph) -> GraphProposalState {
    let mut state = ag_session::test_support::graph_proposal_state("preview", "sess");
    state.version.outcome = GraphProposalVersionOutcome::Generated {
        launch: Some(Box::new(StartExecutionGraphRequest {
            graph: graph.clone(),
            provider_call_limits: std::collections::BTreeMap::new(),
        })),
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Proposal {
                proposal: ag_session::test_support::graph_proposal(),
            },
        },
    };
    state.version.approval = Some(GraphProposalApproval {
        graph_id: graph.id.clone(),
        limits: state.limits.clone(),
        version: 1,
    });

    state
}

/// Serves `proposal` as the latest proposal of every session.
fn serve_proposal(app: &mut App, proposal: Option<GraphProposalState>) {
    let mut proposals = MockGraphProposalRepository::new();
    proposals
        .expect_latest()
        .returning(move |_| Ok(proposal.clone()));
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));
}

/// Serves `graphs` for every session, each with the progress `progress`
/// returns, and the body `Exact output body` for every result.
fn serve_graphs(
    app: &mut App,
    graphs: Vec<ExecutionGraph>,
    progress: impl Fn(&str) -> Option<ExecutionGraphProgress> + Send + Sync + 'static,
) {
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .returning(move |_| Ok(graphs.clone()));
    repository
        .expect_progress()
        .returning(move |graph_id| Ok(progress(graph_id)));
    repository.expect_load_result().returning(|result_id| {
        let (_, progress) = gated_graph();
        let result = progress.nodes[0]
            .results
            .iter()
            .find(|result| result.reference.id == result_id)
            .cloned();

        Ok(result.map(|result| StoredGraphResult {
            acceptance: None,
            body: "Exact output body".into(),
            result,
            validation: None,
        }))
    });
    app.services.set_graph_repository(Arc::new(repository));
}

#[tokio::test]
async fn runtime_backend_reports_no_graph_for_a_session_without_proposals() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;

    // Act
    let session = request_session(&mut app, SessionId::from("sess"))
        .await
        .expect("session should load")
        .expect("session should exist");
    let graphs = request_graphs(&mut app, None, None)
        .await
        .expect("graphs should load");

    // Assert
    assert_eq!(session.graph, None);
    assert_eq!(
        graphs,
        SessionGraphs {
            graph: None,
            graphs: Vec::new(),
            proposal: None,
        }
    );
}

#[tokio::test]
async fn runtime_backend_summarizes_the_approved_graph_in_the_session_snapshot() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let (graph, progress) = gated_graph();
    serve_proposal(&mut app, Some(approved_proposal(&graph)));
    serve_graphs(&mut app, vec![graph], move |_| Some(progress.clone()));

    // Act
    let session = request_session(&mut app, SessionId::from("sess"))
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    // Without a live driver the gated graph reads as paused, as in the
    // terminal's sidebar.
    let summary = session.graph.expect("graph summary");
    assert_eq!(summary.phase, SessionGraphProposalPhase::Approved);
    assert_eq!(summary.status, Some(ExecutionGraphStatus::Paused));
    assert_eq!(summary.label, "Paused");
    assert_eq!((summary.done, summary.total), (1, 2));
    assert_eq!(
        summary
            .awaiting_approval
            .iter()
            .map(|approval| approval.text.as_str())
            .collect::<Vec<_>>(),
        ["first awaits your approval; reply in chat to accept or revise it and resume the graph"]
    );
}

#[tokio::test]
async fn runtime_backend_summarizes_a_generating_proposal_without_reading_a_graph() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    serve_proposal(
        &mut app,
        Some(ag_session::test_support::graph_proposal_state(
            "preview", "sess",
        )),
    );
    // Any graph read fails the test: an unapproved proposal has no graph.
    app.services
        .set_graph_repository(Arc::new(MockGraphRepository::new()));

    // Act
    let session = request_session(&mut app, SessionId::from("sess"))
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    let summary = session.graph.expect("graph summary");
    assert_eq!(summary.phase, SessionGraphProposalPhase::Generating);
    assert_eq!(summary.label, "Generating preview...");
    assert_eq!(summary.status, None);
    assert_eq!((summary.done, summary.total), (0, 0));
}

#[tokio::test]
async fn runtime_backend_keeps_a_snapshot_whose_proposal_cannot_be_read() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let mut proposals = MockGraphProposalRepository::new();
    proposals.expect_latest().returning(|_| {
        Err(DbError::InvalidData {
            entity: "proposal",
            reason: "unavailable".into(),
        })
    });
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));

    // Act
    let session = request_session(&mut app, SessionId::from("sess")).await;
    let graphs = request_graphs(&mut app, None, None).await;

    // Assert
    // The transcript stays readable without the summary; the graph read
    // itself reports the failure.
    let session = session
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(session.graph, None);
    assert!(matches!(graphs, Err(SessionError::Operation(_))));
}

#[tokio::test]
async fn runtime_backend_summarizes_unreadable_progress_as_unavailable() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let (graph, _) = gated_graph();
    serve_proposal(&mut app, Some(approved_proposal(&graph)));
    let mut repository = MockGraphRepository::new();
    repository.expect_progress().returning(|_| {
        Err(DbError::InvalidData {
            entity: "graph",
            reason: "unavailable".into(),
        })
    });
    app.services.set_graph_repository(Arc::new(repository));

    // Act
    let session = request_session(&mut app, SessionId::from("sess"))
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    let summary = session.graph.expect("graph summary");
    assert_eq!(summary.phase, SessionGraphProposalPhase::Approved);
    assert_eq!(summary.label, "Unavailable");
    assert_eq!(summary.status, None);
    assert_eq!((summary.done, summary.total), (0, 2));
}

#[tokio::test]
async fn graph_reads_select_the_approved_graph_and_its_first_node_by_default() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let (graph, progress) = gated_graph();
    serve_proposal(&mut app, Some(approved_proposal(&graph)));
    serve_graphs(&mut app, vec![graph], move |_| Some(progress.clone()));

    // Act
    let graphs = request_graphs(&mut app, None, None)
        .await
        .expect("graphs should load");

    // Assert
    assert_eq!(graphs.graphs.len(), 1);
    assert!(!graphs.graphs[0].is_focused_review);
    let proposal = graphs.proposal.expect("proposal");
    assert_eq!(proposal.phase, SessionGraphProposalPhase::Approved);
    assert_eq!(proposal.hint, None);
    assert!(proposal.preview.starts_with("## Graph proposal v1"));
    let graph = graphs.graph.expect("selected graph");
    assert_eq!(graph.id, "graph");
    assert_eq!(graph.node_id.as_deref(), Some("first"));
    assert_eq!(graph.status, Some(ExecutionGraphStatus::Paused));
    assert_eq!(graph.status_label, "Paused");
    assert_eq!(graph.note, None);
    assert_eq!(graph.snapshot.len(), 16);
    assert_eq!(graph.nodes.len(), 2);
    let (first, second) = (&graph.nodes[0], &graph.nodes[1]);
    assert!(first.awaits_approval);
    assert_eq!(first.status_label, "Awaiting your approval");
    assert_eq!(first.hint, None);
    assert_eq!(second.status_label, "Waiting");
    assert_eq!(
        second.blocked,
        ["first: accepted response", "Paused; resume required"]
    );
    assert_eq!(
        second.requirements,
        [SessionGraphRequirement {
            gate: "accepted response".into(),
            is_satisfied: false,
            label: "needs response accepted".into(),
            node_id: "first".into(),
        }]
    );
    assert_eq!(graph.outputs.len(), 1);
    let output = &graph.outputs[0];
    assert_eq!(output.body, "Exact output body");
    assert_eq!(output.display, "Exact output body");
    assert_eq!(output.result.reference.id, "result");
}

#[tokio::test]
async fn graph_reads_select_the_named_graph_and_node_and_refuse_a_foreign_graph() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let (graph, mut progress) = gated_graph();
    progress.nodes[1].status = GraphNodeStatus::AwaitingAnswers;
    progress.nodes[1].questions = vec![QuestionItem::new("Which branch?")];
    serve_proposal(&mut app, None);
    serve_graphs(&mut app, vec![graph], move |_| Some(progress.clone()));

    // Act
    let named = request_graphs(&mut app, Some("graph"), Some("second"))
        .await
        .expect("graphs should load");
    let unknown_node = request_graphs(&mut app, Some("graph"), Some("missing"))
        .await
        .expect("graphs should load");
    let foreign = request_graphs(&mut app, Some("other-session-graph"), None).await;

    // Assert
    let graph = named.graph.expect("selected graph");
    assert_eq!(graph.node_id.as_deref(), Some("second"));
    assert_eq!(graph.outputs, Vec::new());
    assert_eq!(graph.nodes[1].status_label, "Needs your input");
    assert_eq!(
        graph.nodes[1].hint.as_deref(),
        Some("reply in chat to answer")
    );
    assert_eq!(named.proposal, None);
    assert_eq!(
        unknown_node
            .graph
            .expect("selected graph")
            .node_id
            .as_deref(),
        Some("first")
    );
    assert_eq!(
        foreign,
        Err(SessionError::Operation(
            "Graph does not belong to this session".into()
        ))
    );
}

#[tokio::test]
async fn graph_reads_show_a_pending_proposal_and_a_focused_review_without_progress() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let (graph, progress) = gated_graph();
    let mut review = graph.clone();
    review.id = "review".into();
    review.nodes[0].execution.response_profile = ProtocolRequestProfile::FocusedReview;
    review.nodes[0].execution.workspace_access = GraphWorkspaceAccess::BackgroundRead;
    let mut proposal = ag_session::test_support::graph_proposal_state("preview", "sess");
    proposal.version.outcome = GraphProposalVersionOutcome::Generated {
        launch: None,
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Clarification {
                questions: vec![QuestionItem::new("Which target?")],
            },
        },
    };
    serve_proposal(&mut app, Some(proposal));
    serve_graphs(&mut app, vec![graph, review], move |graph_id| {
        (graph_id == "graph").then(|| progress.clone())
    });

    // Act
    let graphs = request_graphs(&mut app, Some("review"), None)
        .await
        .expect("graphs should load");

    // Assert
    let proposal = graphs.proposal.expect("proposal");
    assert_eq!(proposal.phase, SessionGraphProposalPhase::NeedsInput);
    assert_eq!(
        proposal.hint.as_deref(),
        Some("reply in chat to approve, revise, or cancel")
    );
    assert_eq!(proposal.questions, [QuestionItem::new("Which target?")]);
    assert_eq!(
        graphs
            .graphs
            .iter()
            .map(|entry| (entry.id.as_str(), entry.is_focused_review))
            .collect::<Vec<_>>(),
        [("graph", false), ("review", true)]
    );
    let review = graphs.graph.expect("selected graph");
    assert!(review.is_focused_review);
    assert!(
        review
            .note
            .as_deref()
            .is_some_and(|note| note.starts_with("Focused review:"))
    );
    assert_eq!(review.status, None);
    assert_eq!(review.status_label, "Unavailable");
    assert_eq!(review.node_id, None);
    assert_eq!(review.nodes[0].progress, None);
    assert_eq!(review.nodes[0].status_label, "Waiting");
}

#[tokio::test]
async fn graph_snapshot_tokens_change_only_when_the_graph_progress_does() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let (graph, progress) = gated_graph();
    let served = Arc::new(Mutex::new(progress.clone()));
    let current = Arc::clone(&served);
    serve_proposal(&mut app, None);
    serve_graphs(&mut app, vec![graph], move |_| {
        Some(current.lock().expect("progress").clone())
    });

    // Act
    let first = request_graphs(&mut app, None, None).await;
    let again = request_graphs(&mut app, None, Some("second")).await;
    served.lock().expect("progress").nodes[1].status = GraphNodeStatus::Running;
    let changed = request_graphs(&mut app, None, None).await;

    // Assert
    let token = |graphs: Result<SessionGraphs, SessionError>| {
        graphs
            .expect("graphs should load")
            .graph
            .expect("selected graph")
            .snapshot
    };
    let first = token(first);
    assert_eq!(token(again), first);
    assert_ne!(token(changed), first);
}
