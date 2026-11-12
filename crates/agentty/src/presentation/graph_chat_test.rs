use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ag_protocol::{GraphProposalOutcome, GraphProposalResponse, QuestionItem};
use ag_session::{
    ExecutionGraph, GraphProposalResolution, GraphProposalVersionOutcome,
    StartExecutionGraphRequest, test_support,
};

use super::{GraphChatCache, GraphChatSnapshot, proposal_chat_notice};

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
