//! Graph chat snapshot cache refresh and proposal-outcome notice unit tests.

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use ag_protocol::{GraphProposalOutcome, GraphProposalResponse, QuestionItem};
use ag_session::{
    ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus, GraphCompletionGate,
    GraphDependency, GraphNodeProgress, GraphNodeStatus, GraphProposalApproval,
    GraphProposalResolution, GraphProposalVersionOutcome, SessionGraphApproval,
    SessionGraphProposalPhase, SessionGraphSummary, StartExecutionGraphRequest, test_support,
};

use super::{
    GraphChatCache, GraphChatSnapshot, edge_label, graph_summary, output_approval_notice,
    proposal_chat_notice, proposal_standing,
};

#[test]
fn cache_is_session_scoped_and_refreshes_on_updates_live_status_and_failures() {
    // Arrange
    let mut cache = GraphChatCache::default();
    let now = Instant::now();
    let snapshot = GraphChatSnapshot {
        error: None,
        progress: None,
        proposal: test_support::graph_proposal_state("proposal", "session"),
    };

    // Act / Assert
    assert!(cache.needs_refresh("session", 1, now));
    assert!(!cache.complete("session".into(), 1, now, Ok(None)));
    assert!(!cache.needs_refresh("session", 1, now + Duration::from_secs(2)));
    assert!(cache.needs_refresh("session", 2, now));
    assert!(cache.complete("session".into(), 2, now, Ok(Some(snapshot.clone()))));
    assert_eq!(cache.snapshot("session"), Some(&snapshot));
    assert!(cache.snapshot("other").is_none());
    assert!(!cache.needs_refresh("session", 2, now));
    assert!(cache.needs_refresh("session", 2, now + Duration::from_secs(1)));
    assert!(cache.complete("session".into(), 2, now, Err("offline".into())));
    assert_eq!(
        cache
            .snapshot("session")
            .expect("retained snapshot")
            .error
            .as_deref(),
        Some("offline")
    );
    assert!(cache.complete("session".into(), 2, now, Ok(Some(snapshot.clone()))));
    assert!(!cache.complete("session".into(), 2, now, Ok(Some(snapshot))));
    assert!(cache.needs_refresh("other", 2, now));
    assert!(cache.complete("other".into(), 2, now, Err("offline".into())));
    assert!(cache.snapshot("session").is_none());
    assert!(cache.snapshot("other").is_none());
    assert!(cache.needs_refresh("other", 2, now + Duration::from_secs(1)));
}

#[test]
fn snapshot_animates_only_while_a_preview_generates_or_graph_work_runs() {
    // Arrange
    let mut snapshot = GraphChatSnapshot {
        error: None,
        progress: None,
        proposal: test_support::graph_proposal_state("proposal", "session"),
    };
    let generating = snapshot.is_animating();
    snapshot.proposal.version.is_cancelled = true;
    let cancelled = snapshot.is_animating();
    snapshot.proposal.version.is_cancelled = false;
    snapshot.proposal.version.outcome = GraphProposalVersionOutcome::Failed("invalid".into());
    let failed = snapshot.is_animating();
    let mut progress = ExecutionGraphProgress {
        graph_id: "graph".into(),
        nodes: Vec::new(),
        revision: NonZeroU64::MIN,
        session_id: "session".into(),
        status: ExecutionGraphStatus::Running,
    };
    snapshot.progress = Some(progress.clone());

    // Act
    let running = snapshot.is_animating();
    progress.status = ExecutionGraphStatus::Failed;
    progress.nodes = [GraphNodeStatus::Failed, GraphNodeStatus::Running]
        .into_iter()
        .map(|status| GraphNodeProgress {
            blocked: Vec::new(),
            executions: Vec::new(),
            last_error: None,
            node_id: format!("{status:?}"),
            questions: Vec::new(),
            results: Vec::new(),
            status,
        })
        .collect();
    snapshot.progress = Some(progress.clone());
    let failed_with_running_sibling = snapshot.is_animating();
    progress.nodes.pop();
    snapshot.progress = Some(progress.clone());
    let failed_settled = snapshot.is_animating();
    progress.status = ExecutionGraphStatus::Completed;
    snapshot.progress = Some(progress);
    let completed = snapshot.is_animating();

    // Assert
    assert!(generating);
    assert!(!cancelled);
    assert!(!failed);
    assert!(running);
    assert!(failed_with_running_sibling);
    assert!(!failed_settled);
    assert!(!completed);
}

#[test]
fn cache_invalidation_refreshes_an_empty_session_once() {
    // Arrange
    let mut cache = GraphChatCache::default();
    let now = Instant::now();
    cache.complete("session".into(), 1, now, Ok(None));

    // Act
    cache.invalidate();

    // Assert
    assert!(cache.needs_refresh("session", 1, now));
    assert!(!cache.complete("session".into(), 1, now, Ok(None)));
    assert!(!cache.needs_refresh("session", 1, now + Duration::from_secs(2)));
}

#[test]
fn chat_notice_reports_each_proposal_outcome_in_one_short_message() {
    // Arrange
    let mut state = test_support::graph_proposal_state("proposal", "session");
    let declaration = test_support::graph_proposal();
    let graph = GraphProposalResolution {
        agent: state.agent,
        answers: &state.version.answers,
        definitions: &state.definitions,
        graph_id: "graph",
        limits: &state.limits,
        permission: state.permission,
        request: &state.input.request,
        revision_attachments: &state.version.attachments,
        session_id: &state.input.session_id,
    }
    .resolve(&declaration)
    .expect("resolved graph");
    let mut second = graph.clone();
    second.nodes.push(graph.nodes[0].clone());
    let generated =
        |launch: Option<ExecutionGraph>, outcome| GraphProposalVersionOutcome::Generated {
            launch: launch.map(|graph| {
                Box::new(StartExecutionGraphRequest {
                    graph,
                    provider_call_limits: BTreeMap::new(),
                })
            }),
            response: GraphProposalResponse { outcome },
        };
    let proposal = || GraphProposalOutcome::Proposal {
        proposal: declaration.clone(),
    };
    let cases = [
        (
            GraphProposalVersionOutcome::Generating,
            "Generating graph proposal v1…",
        ),
        (
            GraphProposalVersionOutcome::Failed("invalid graph".into()),
            "Graph proposal v1 failed: invalid graph. Reply to revise or cancel it.",
        ),
        (
            generated(
                None,
                GraphProposalOutcome::Clarification {
                    questions: vec![
                        QuestionItem::new("Which target?"),
                        QuestionItem::with_options("Scope?", vec!["All".into(), "Docs".into()]),
                    ],
                },
            ),
            "Graph proposal v1 needs your input:\n1. Which target?\n2. Scope? (All / Docs)",
        ),
        (
            generated(None, proposal()),
            "Graph proposal v1 is unresolved. Reply to revise or cancel it.",
        ),
        (
            generated(Some(graph), proposal()),
            "Graph proposal v1 ready: 1 node. Reply to approve, revise, or cancel it.",
        ),
        (
            generated(Some(second), proposal()),
            "Graph proposal v1 ready: 2 nodes. Reply to approve, revise, or cancel it.",
        ),
    ];

    // Act / Assert
    for (outcome, expected) in cases {
        state.version.outcome = outcome;
        assert_eq!(proposal_chat_notice(&state), expected);
    }
    state.version.is_cancelled = true;
    assert_eq!(proposal_chat_notice(&state), "Graph proposal v1 cancelled.");
}

#[test]
fn output_approval_notice_shows_the_output_and_the_reply_guidance() {
    // Arrange
    let cases = [
        (
            "Plan body\n",
            true,
            "Plan body\n\n`plan` awaits your approval; reply in chat to accept or revise it.",
        ),
        (
            " ",
            false,
            "`plan` awaits your approval; reply in chat to accept it.",
        ),
    ];

    // Act / Assert
    for (output, takes_revision, expected) in cases {
        assert_eq!(
            output_approval_notice("plan", output, takes_revision),
            expected
        );
    }
}

/// Returns a proposal outcome that resolved to `graph`, or that the host
/// could not resolve without one.
fn resolved_outcome(graph: Option<ExecutionGraph>) -> GraphProposalVersionOutcome {
    GraphProposalVersionOutcome::Generated {
        launch: graph.map(|graph| {
            Box::new(StartExecutionGraphRequest {
                graph,
                provider_call_limits: BTreeMap::new(),
            })
        }),
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Proposal {
                proposal: test_support::graph_proposal(),
            },
        },
    }
}

#[test]
fn proposal_standing_names_each_phase_with_the_sidebar_row() {
    // Arrange
    let mut state = test_support::graph_proposal_state("proposal", "session");
    let graph = crate::presentation::execution_graph::tests::graph_view().graphs[0].clone();
    let cases = [
        (
            GraphProposalVersionOutcome::Generating,
            SessionGraphProposalPhase::Generating,
            "Generating preview...",
        ),
        (
            GraphProposalVersionOutcome::Failed("invalid graph".into()),
            SessionGraphProposalPhase::Failed,
            "Generation failed: invalid graph",
        ),
        (
            GraphProposalVersionOutcome::Generated {
                launch: None,
                response: GraphProposalResponse {
                    outcome: GraphProposalOutcome::Clarification {
                        questions: vec![QuestionItem::new("Which target?")],
                    },
                },
            },
            SessionGraphProposalPhase::NeedsInput,
            "Needs your input",
        ),
        (
            resolved_outcome(None),
            SessionGraphProposalPhase::Unresolved,
            "Unresolved. Reply to revise or cancel it.",
        ),
        (
            resolved_outcome(Some(graph)),
            SessionGraphProposalPhase::AwaitingApproval,
            "Awaiting your approval",
        ),
    ];

    // Act / Assert
    for (outcome, phase, label) in cases {
        state.version.outcome = outcome;
        assert_eq!(proposal_standing(&state), (phase, label.to_string()));
    }
    state.version.approval = Some(GraphProposalApproval {
        graph_id: "graph".into(),
        limits: state.limits.clone(),
        version: 1,
    });
    assert_eq!(
        proposal_standing(&state),
        (
            SessionGraphProposalPhase::Approved,
            "Approved; loading execution status".to_string()
        )
    );
    state.version.is_cancelled = true;
    assert_eq!(
        proposal_standing(&state),
        (
            SessionGraphProposalPhase::Cancelled,
            "Cancelled. No graph was launched.".to_string()
        )
    );
}

#[test]
fn graph_summary_counts_nodes_and_names_outputs_awaiting_approval() {
    // Arrange
    let view = crate::presentation::execution_graph::tests::acceptance_gated_view();
    let graph = view.graphs[0].clone();
    let mut progress = view.progress.expect("progress");
    let mut state = test_support::graph_proposal_state("proposal", "session");
    state.version.outcome = resolved_outcome(Some(graph));
    state.version.approval = Some(GraphProposalApproval {
        graph_id: "graph".into(),
        limits: state.limits.clone(),
        version: 1,
    });

    // Act
    let starting = graph_summary(&state, None);
    let blocked = graph_summary(&state, Some(&progress));
    progress.status = ExecutionGraphStatus::Paused;
    let paused = graph_summary(&state, Some(&progress));
    state.version.is_cancelled = true;
    let cancelled = graph_summary(&state, Some(&progress));

    // Assert
    assert_eq!(
        starting,
        SessionGraphSummary {
            awaiting_approval: Vec::new(),
            done: 0,
            label: "Approved; loading execution status".into(),
            phase: SessionGraphProposalPhase::Approved,
            status: None,
            total: 2,
            version: 1,
        }
    );
    assert_eq!(
        blocked,
        SessionGraphSummary {
            awaiting_approval: vec![SessionGraphApproval {
                node_id: "first".into(),
                takes_revision: true,
                text: "first awaits your approval; reply in chat to accept or revise it".into(),
            }],
            done: 1,
            label: "Blocked".into(),
            phase: SessionGraphProposalPhase::Approved,
            status: Some(ExecutionGraphStatus::Blocked),
            total: 2,
            version: 1,
        }
    );
    assert_eq!(
        paused.awaiting_approval[0].text,
        "first awaits your approval; reply in chat to accept or revise it and resume the graph"
    );
    assert_eq!(paused.label, "Paused");
    assert_eq!(
        cancelled,
        SessionGraphSummary {
            awaiting_approval: Vec::new(),
            done: 0,
            label: "Cancelled. No graph was launched.".into(),
            phase: SessionGraphProposalPhase::Cancelled,
            status: None,
            total: 0,
            version: 1,
        }
    );
}

#[test]
fn dependency_labels_name_their_source_unless_it_is_drawn_above() {
    // Arrange
    let output = |output_name: &str, accepted: bool| GraphDependency {
        gate: if accepted {
            GraphCompletionGate::ResultAccepted {
                output_name: output_name.into(),
            }
        } else {
            GraphCompletionGate::ResultValidated {
                output_name: output_name.into(),
            }
        },
        node_id: "plan".into(),
    };
    let after = GraphDependency {
        gate: GraphCompletionGate::ExecutionSucceeded,
        node_id: "plan".into(),
    };

    // Act / Assert
    assert_eq!(edge_label(&after, Some("plan")), "after plan");
    assert_eq!(
        edge_label(&output("report", false), Some("plan")),
        "needs report"
    );
    assert_eq!(
        edge_label(&output("report", true), Some("other")),
        "needs report accepted from plan"
    );
    assert_eq!(
        edge_label(&output("report", false), None),
        "needs report from plan"
    );
}
