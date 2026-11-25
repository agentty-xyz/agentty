//! Session API graph action tests: the actions a graph read offers, which
//! match the terminal inspector's with the same node and output selected,
//! and each action's admission and refusals.

use std::sync::Arc;

use ag_protocol::ProtocolRequestProfile;
use ag_session::{
    ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus, ExtendGraphBudgetRequest,
    GraphBlockedReason, GraphNodeContinuation, GraphNodeStatus, GraphWorkspaceAccess, SessionError,
    SessionGraph, SessionGraphAction, SessionGraphActionKind, SessionGraphActionRequest, SessionId,
};
use ag_store::MockGraphRepository;

use super::{gated_graph, graph_repository, request_graphs, serve_proposal};
use crate::app::App;
use crate::presentation::app_mode::ConfirmationViewMode;
use crate::presentation::execution_graph::GraphView;
use crate::test_support::new_test_app;

/// Sends `action` on `graph_id` of `sess`, bound to `snapshot`, through the
/// session API.
async fn act(
    app: &mut App,
    graph_id: &str,
    action: SessionGraphAction,
    snapshot: &str,
) -> Result<(), SessionError> {
    let service = app.session_service();
    let request = SessionGraphActionRequest {
        action,
        graph_id: graph_id.to_string(),
        session_id: SessionId::from("sess"),
        snapshot: snapshot.to_string(),
    };

    app.drive_session_request(async move { service.act_on_session_graph(request).await })
        .await
}

/// The selected graph of a read of `graph` with `node_id` selected.
async fn read_graph(app: &mut App, node_id: &str) -> SessionGraph {
    request_graphs(app, Some("graph"), Some(node_id))
        .await
        .expect("graphs should load")
        .graph
        .expect("selected graph")
}

/// Serves `graph` with `progress` and no proposal, expecting of the
/// repository what `expect` adds; any other mutation fails the test.
fn serve(
    app: &mut App,
    graph: ExecutionGraph,
    progress: ExecutionGraphProgress,
    expect: impl FnOnce(&mut MockGraphRepository),
) {
    serve_proposal(app, None);
    let mut repository = graph_repository(vec![graph], move |_| Some(progress.clone()));
    expect(&mut repository);
    app.services.set_graph_repository(Arc::new(repository));
}

/// [`gated_graph`] changed as `variant` names: `first` failed, `second`
/// waits for the spent `calls` budget, a pending `third` node also waits
/// for the spent `other` budget, the graph was cancelled, or it is a
/// focused review.
fn graph_variant(variant: &str) -> (ExecutionGraph, ExecutionGraphProgress) {
    let (mut graph, mut progress) = gated_graph();
    match variant {
        "failed" => progress.nodes[0].status = GraphNodeStatus::Failed,
        "budget" => progress.nodes[1]
            .blocked
            .push(GraphBlockedReason::BudgetExhausted("calls".into())),
        "budgets" => {
            progress.nodes[1]
                .blocked
                .push(GraphBlockedReason::BudgetExhausted("calls".into()));
            let mut third = graph.nodes[1].clone();
            third.id = "third".into();
            graph.nodes.push(third);
            let mut third_progress = progress.nodes[1].clone();
            third_progress.node_id = "third".into();
            third_progress.blocked = vec![GraphBlockedReason::BudgetExhausted("other".into())];
            progress.nodes.push(third_progress);
        }
        "cancelled" => progress.status = ExecutionGraphStatus::Cancelled,
        "focused-review" => {
            graph.nodes[0].execution.response_profile = ProtocolRequestProfile::FocusedReview;
            graph.nodes[0].execution.workspace_access = GraphWorkspaceAccess::BackgroundRead;
        }
        _ => {}
    }

    (graph, progress)
}

/// The actions whose flag is set, in the given order.
fn offered<const COUNT: usize>(
    actions: [(SessionGraphActionKind, bool); COUNT],
) -> Vec<SessionGraphActionKind> {
    actions
        .into_iter()
        .filter_map(|(action, is_offered)| is_offered.then_some(action))
        .collect()
}

#[tokio::test]
async fn graph_reads_offer_what_the_terminal_inspector_offers_with_each_node_selected() {
    for variant in [
        "gated",
        "failed",
        "budget",
        "budgets",
        "cancelled",
        "focused-review",
    ] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        let (graph, progress) = graph_variant(variant);
        let node_ids = graph
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        serve(&mut app, graph, progress, |_| {});

        for (index, node_id) in node_ids.iter().enumerate() {
            // Act
            let read = read_graph(&mut app, node_id).await;
            let mut view = GraphView::new(ConfirmationViewMode {
                session_id: "sess".into(),
                scroll_offset: None,
            });
            view.is_proposal_selected = false;
            view.selected_node = index;
            app.load_graph_view(&mut view)
                .await
                .expect("view should load");

            // Assert
            let context = format!("{variant}: {node_id}");
            assert_eq!(
                read.actions,
                offered([
                    (SessionGraphActionKind::Cancel, view.can_cancel()),
                    (
                        SessionGraphActionKind::ExtendBudget,
                        view.can_extend_budget()
                    ),
                    (SessionGraphActionKind::Resume, view.can_resume()),
                ]),
                "{context}"
            );
            assert_eq!(
                read.exhausted_budget.as_deref(),
                view.exhausted_budget().filter(|_| view.can_extend_budget()),
                "{context}"
            );
            assert_eq!(
                read.nodes[index].actions,
                offered([
                    (SessionGraphActionKind::Reply, view.can_continue()),
                    (SessionGraphActionKind::Retry, view.can_retry()),
                ]),
                "{context}"
            );
            assert_eq!(read.outputs.len(), view.outputs.len(), "{context}");
            for (output_index, output) in read.outputs.iter().enumerate() {
                view.selected_output = output_index;
                assert_eq!(
                    output.actions,
                    offered([(SessionGraphActionKind::Accept, view.can_accept())]),
                    "{context}"
                );
            }
        }
    }
}

#[tokio::test]
async fn graph_reads_name_the_actions_each_part_of_a_graph_takes() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let (graph, progress) = graph_variant("budget");
    serve(&mut app, graph, progress, |_| {});

    // Act
    let read = read_graph(&mut app, "first").await;

    // Assert
    // Without a live driver the graph reads as paused, so it also resumes.
    assert_eq!(
        read.actions,
        [
            SessionGraphActionKind::Cancel,
            SessionGraphActionKind::ExtendBudget,
            SessionGraphActionKind::Resume,
        ]
    );
    assert_eq!(read.exhausted_budget.as_deref(), Some("calls"));
    assert_eq!(read.nodes[0].actions, [SessionGraphActionKind::Reply]);
    assert_eq!(read.nodes[1].actions, Vec::new());
    assert_eq!(read.outputs[0].actions, [SessionGraphActionKind::Accept]);
}

#[tokio::test]
async fn each_offered_action_admits_the_mutation_the_terminal_inspector_admits() {
    for name in [
        "accept",
        "cancel",
        "extend_budget",
        "reply",
        "resume",
        "retry",
    ] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        let (graph, progress) = graph_variant(match name {
            "extend_budget" => "budget",
            "retry" => "failed",
            _ => "gated",
        });
        serve(&mut app, graph, progress, |repository| match name {
            "accept" => {
                repository
                    .expect_accept_result()
                    .once()
                    .withf(|reference, _| reference.id == "result" && reference.node_id == "first")
                    .return_once(|_, _| Ok(true));
            }
            "cancel" => {
                repository
                    .expect_cancel()
                    .once()
                    .withf(|request| request.graph_id == "graph")
                    .return_once(|_| Ok(()));
            }
            "extend_budget" => {
                repository
                    .expect_extend_budget()
                    .once()
                    .withf(|request| {
                        request.budget_id == "calls"
                            && request.additional_calls == 4
                            && request.graph.graph_id == "graph"
                    })
                    .return_once(|_| Ok(true));
            }
            "resume" => {
                // The graph went away after the snapshot was read, which
                // only the resume path reads it for.
                repository
                    .expect_load_graph()
                    .once()
                    .return_once(|_, _| Ok(None));
            }
            _ => {
                repository
                    .expect_continue_node()
                    .once()
                    .withf(move |request| {
                        request.node_id == "first"
                            && match &request.kind {
                                GraphNodeContinuation::Retry => name == "retry",
                                GraphNodeContinuation::Reply { message, .. } => {
                                    name == "reply" && message == "Shorter, please"
                                }
                                GraphNodeContinuation::Answer { .. } => false,
                            }
                    })
                    .return_once(|_| Ok(true));
            }
        });
        let snapshot = read_graph(&mut app, "first").await.snapshot;
        let action = match name {
            "accept" => SessionGraphAction::Accept {
                node_id: "first".into(),
                output_id: "result".into(),
            },
            "cancel" => SessionGraphAction::Cancel,
            "extend_budget" => SessionGraphAction::ExtendBudget {
                additional_calls: 4,
                budget_id: "calls".into(),
            },
            "reply" => SessionGraphAction::Reply {
                message: "  Shorter, please \n".into(),
                node_id: "first".into(),
            },
            "resume" => SessionGraphAction::Resume,
            _ => SessionGraphAction::Retry {
                node_id: "first".into(),
            },
        };

        // Act
        let result = act(&mut app, "graph", action, &snapshot).await;

        // Assert
        if name == "resume" {
            assert_eq!(
                result,
                Err(SessionError::Operation("Missing or stale graph".into()))
            );
        } else {
            assert_eq!(result, Ok(()), "{name}");
        }
    }
}

#[tokio::test]
async fn graph_reads_name_the_selected_nodes_spent_budget_and_extensions_extend_the_named_one() {
    for budget_id in ["calls", "other"] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        let (mut graph, progress) = graph_variant("budgets");
        // `first` shares a budget that is not spent.
        graph.nodes[0].execution.shared_budget_id = Some("idle".into());
        let expected = budget_id.to_string();
        serve(&mut app, graph, progress, move |repository| {
            repository
                .expect_extend_budget()
                .once()
                .withf(move |request| {
                    request.budget_id == expected
                        && request.additional_calls == 4
                        && request.graph.graph_id == "graph"
                })
                .return_once(|_| Ok(true));
        });
        // The read with `first` selected names the first spent budget.
        let snapshot = read_graph(&mut app, "first").await.snapshot;
        let extend = |additional_calls: usize, budget_id: &str| SessionGraphAction::ExtendBudget {
            additional_calls,
            budget_id: budget_id.into(),
        };
        let max = ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS;

        // Act
        let selected = [
            read_graph(&mut app, "first").await.exhausted_budget,
            read_graph(&mut app, "second").await.exhausted_budget,
            read_graph(&mut app, "third").await.exhausted_budget,
        ];
        let mut refused = Vec::new();
        for action in [
            extend(0, budget_id),
            extend(max + 1, budget_id),
            extend(4, "missing"),
            extend(4, "idle"),
        ] {
            refused.push(act(&mut app, "graph", action, &snapshot).await);
        }
        let extended = act(&mut app, "graph", extend(4, budget_id), &snapshot).await;

        // Assert
        assert_eq!(
            selected,
            [
                Some("calls".to_string()),
                Some("calls".to_string()),
                Some("other".to_string()),
            ]
        );
        let out_of_range = format!("Enter a whole number of provider calls from 1 to {max}");
        let unavailable = "Action unavailable for this snapshot".to_string();
        assert_eq!(
            refused,
            [
                out_of_range.clone(),
                out_of_range,
                unavailable.clone(),
                unavailable
            ]
            .map(|message| Err(SessionError::Operation(message))),
            "{budget_id}"
        );
        assert_eq!(extended, Ok(()), "{budget_id}");
    }
}

#[tokio::test]
async fn graph_actions_refuse_foreign_graphs_stale_snapshots_and_what_is_not_offered() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let (graph, progress) = graph_variant("budget");
    // Every case is refused before it reaches the repository.
    serve(&mut app, graph, progress, |repository| {
        repository.expect_accept_result().never();
        repository.expect_continue_node().never();
        repository.expect_extend_budget().never();
    });
    let snapshot = read_graph(&mut app, "first").await.snapshot;
    let retry = |node_id: &str| SessionGraphAction::Retry {
        node_id: node_id.into(),
    };
    let cases = [
        (
            "other-session-graph",
            SessionGraphAction::Cancel,
            snapshot.as_str(),
            "Graph does not belong to this session".to_string(),
        ),
        (
            "graph",
            SessionGraphAction::Cancel,
            "stale",
            "Graph changed; refresh the view and inspect it before acting again.".to_string(),
        ),
        // Stale snapshots are refused before the action is checked.
        (
            "graph",
            retry("first"),
            "stale",
            "Graph changed; refresh the view and inspect it before acting again.".to_string(),
        ),
        (
            "graph",
            retry("first"),
            snapshot.as_str(),
            "Action unavailable for this snapshot".to_string(),
        ),
        // An unknown node never falls back to `first`, which takes both.
        (
            "graph",
            SessionGraphAction::Reply {
                message: "Look again".into(),
                node_id: "missing".into(),
            },
            snapshot.as_str(),
            "Action unavailable for this snapshot".to_string(),
        ),
        (
            "graph",
            SessionGraphAction::Accept {
                node_id: "missing".into(),
                output_id: "result".into(),
            },
            snapshot.as_str(),
            "Action unavailable for this snapshot".to_string(),
        ),
        (
            "graph",
            SessionGraphAction::Accept {
                node_id: "first".into(),
                output_id: "missing".into(),
            },
            snapshot.as_str(),
            "Action unavailable for this snapshot".to_string(),
        ),
        // `second` produced no output to accept.
        (
            "graph",
            SessionGraphAction::Accept {
                node_id: "second".into(),
                output_id: "result".into(),
            },
            snapshot.as_str(),
            "Action unavailable for this snapshot".to_string(),
        ),
        // No node of this snapshot waits for `other`.
        (
            "graph",
            SessionGraphAction::ExtendBudget {
                additional_calls: 4,
                budget_id: "other".into(),
            },
            snapshot.as_str(),
            "Action unavailable for this snapshot".to_string(),
        ),
        (
            "graph",
            SessionGraphAction::Reply {
                message: " \n ".into(),
                node_id: "first".into(),
            },
            snapshot.as_str(),
            "Enter a reply".to_string(),
        ),
    ];

    for (graph_id, action, token, expected) in cases {
        // Act
        let context = format!("{action:?} on {graph_id} at {token}");
        let result = act(&mut app, graph_id, action, token).await;

        // Assert
        // The repository expects no mutation, so none was admitted.
        assert_eq!(result, Err(SessionError::Operation(expected)), "{context}");
    }
}

#[tokio::test]
async fn focused_review_graphs_take_only_cancellation_of_the_generic_actions() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let (graph, mut progress) = graph_variant("focused-review");
    progress.nodes[0].status = GraphNodeStatus::Failed;
    serve(&mut app, graph, progress, |repository| {
        repository
            .expect_cancel()
            .once()
            .withf(|request| request.graph_id == "graph")
            .return_once(|_| Ok(()));
    });
    let read = read_graph(&mut app, "first").await;
    let refused = [
        SessionGraphAction::Resume,
        SessionGraphAction::Retry {
            node_id: "first".into(),
        },
        SessionGraphAction::Reply {
            message: "Look again".into(),
            node_id: "first".into(),
        },
    ];

    // Act
    let mut results = Vec::new();
    for action in refused {
        results.push(act(&mut app, "graph", action, &read.snapshot).await);
    }
    let cancelled = act(
        &mut app,
        "graph",
        SessionGraphAction::Cancel,
        &read.snapshot,
    )
    .await;

    // Assert
    assert_eq!(read.actions, [SessionGraphActionKind::Cancel]);
    assert_eq!(read.nodes[0].actions, Vec::new());
    assert_eq!(
        results,
        vec![
            Err(SessionError::Operation(
                "Action unavailable for this snapshot".into()
            ));
            3
        ]
    );
    assert_eq!(cancelled, Ok(()));
}
