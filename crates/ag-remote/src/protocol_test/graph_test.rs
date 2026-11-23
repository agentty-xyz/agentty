//! Graph frames: the summary a session snapshot carries and the
//! `get_session_graphs` read, with their conversion, encoding, budgets,
//! offered actions, and parsing from older servers, and the
//! `act_on_session_graph` request.

use ag_session::{
    ExecutionGraphStatus, ExtendGraphBudgetRequest, GraphNodeStatus, QuestionItem, SessionError,
    SessionGraphAction, SessionGraphActionKind, SessionGraphActionRequest, SessionGraphApproval,
    SessionGraphProposalPhase, SessionGraphSummary, SessionGraphs, SessionId,
};
use serde_json::json;

use crate::protocol::{
    MAX_FILE_RESPONSE_BYTES, RemoteGraph, RemoteGraphAction, RemoteGraphApproval, RemoteGraphEntry,
    RemoteGraphNode, RemoteGraphNodeStatus, RemoteGraphOutput, RemoteGraphOutputState,
    RemoteGraphProposal, RemoteGraphProposalPhase, RemoteGraphRequirement, RemoteGraphStatus,
    RemoteQuestion, RemoteRequest, RemoteResponse, RemoteSession, RemoteSessionGraph,
};
use crate::test_support::session_fixture;

/// The parts of a `graphs` response, all empty for any other response.
#[derive(Default)]
struct GraphsParts {
    graph: Option<Box<RemoteGraph>>,
    graphs: Vec<RemoteGraphEntry>,
    proposal: Option<Box<RemoteGraphProposal>>,
}

/// Returns the parts of a `graphs` response.
fn graphs_parts(response: RemoteResponse) -> GraphsParts {
    match response {
        RemoteResponse::Graphs {
            graph,
            graphs,
            proposal,
            ..
        } => GraphsParts {
            graph,
            graphs,
            proposal,
        },
        _ => GraphsParts::default(),
    }
}

#[test]
fn session_conversion_carries_the_graph_summary() {
    // Arrange
    let mut session = session_fixture(&SessionId::from("session-1"));
    session.graph = Some(SessionGraphSummary {
        awaiting_approval: vec![SessionGraphApproval {
            node_id: "inspect".to_string(),
            takes_revision: true,
            text: "inspect awaits your approval; reply in chat to accept or revise it".to_string(),
        }],
        done: 1,
        label: "Running".to_string(),
        phase: SessionGraphProposalPhase::Approved,
        status: Some(ExecutionGraphStatus::Running),
        total: 3,
        version: 2,
    });

    // Act
    let remote = RemoteSession::from(session);
    let encoded = serde_json::to_value(&remote).expect("session should encode");
    let decoded: RemoteSession =
        serde_json::from_value(encoded.clone()).expect("session should decode");

    // Assert
    assert_eq!(
        remote.graph,
        Some(RemoteSessionGraph {
            awaiting: vec![RemoteGraphApproval {
                node_id: "inspect".to_string(),
                revisable: true,
                text: "inspect awaits your approval; reply in chat to accept or revise it"
                    .to_string(),
            }],
            done: 1,
            label: "Running".to_string(),
            phase: RemoteGraphProposalPhase::Approved,
            status: Some(RemoteGraphStatus::Running),
            total: 3,
            version: 2,
        })
    );
    assert_eq!(
        encoded["graph"],
        json!({
            "awaiting": [{
                "node_id": "inspect",
                "revisable": true,
                "text": "inspect awaits your approval; reply in chat to accept or revise it"
            }],
            "done": 1,
            "label": "Running",
            "phase": "approved",
            "status": "running",
            "total": 3,
            "version": 2
        })
    );
    assert_eq!(decoded, remote);
}

#[test]
fn session_graph_summaries_omit_what_a_pending_proposal_lacks() {
    // Arrange
    let mut session = session_fixture(&SessionId::from("session-1"));
    session.graph = Some(SessionGraphSummary {
        awaiting_approval: Vec::new(),
        done: 0,
        label: "Generating preview...".to_string(),
        phase: SessionGraphProposalPhase::Generating,
        status: None,
        total: 0,
        version: 1,
    });

    // Act
    let encoded =
        serde_json::to_value(RemoteSession::from(session)).expect("session should encode");
    let without_graph = serde_json::to_value(RemoteSession::from(session_fixture(
        &SessionId::from("session-1"),
    )))
    .expect("session should encode");

    // Assert
    assert_eq!(
        encoded["graph"],
        json!({
            "done": 0,
            "label": "Generating preview...",
            "phase": "generating",
            "total": 0,
            "version": 1
        })
    );
    assert!(without_graph.get("graph").is_none());
}

#[test]
fn graph_requests_name_only_the_selection_they_make() {
    // Arrange
    let default = RemoteRequest::GetSessionGraphs {
        graph_id: None,
        known: None,
        node_id: None,
        session_id: "s".to_string(),
    };
    let selected = RemoteRequest::GetSessionGraphs {
        graph_id: Some("graph".to_string()),
        known: Some("v1".to_string()),
        node_id: Some("fix".to_string()),
        session_id: "s".to_string(),
    };

    // Act
    let encoded = [
        serde_json::to_value(&default).expect("request should encode"),
        serde_json::to_value(&selected).expect("request should encode"),
    ];
    // An older page names no known version.
    let parsed: RemoteRequest =
        serde_json::from_value(json!({"method": "get_session_graphs", "session_id": "s"}))
            .expect("request should parse");

    // Assert
    assert_eq!(
        encoded,
        [
            json!({"method": "get_session_graphs", "session_id": "s"}),
            json!({
                "method": "get_session_graphs",
                "graph_id": "graph",
                "known": "v1",
                "node_id": "fix",
                "session_id": "s"
            }),
        ]
    );
    assert_eq!(parsed, default);
}

#[test]
fn graph_versions_follow_the_content_and_an_unchanged_answer_carries_only_its_version() {
    // Arrange
    let graphs = ag_session::test_support::session_graphs("session-1");
    let mut changed = graphs.clone();
    if let Some(graph) = changed.graph.as_mut() {
        graph.snapshot.push_str("-next");
    }

    // Act
    let full = RemoteResponse::from_graphs(graphs.clone());
    let again = RemoteResponse::from_graphs(graphs);
    let other = RemoteResponse::from_graphs(changed);
    let version = |response: &RemoteResponse| match response {
        RemoteResponse::Graphs { version, .. } => version.clone(),
        _ => String::new(),
    };
    let shown = version(&full);
    let unchanged = full.clone().unchanged_since(Some(&shown));
    let stale = full.clone().unchanged_since(Some("stale"));
    let accepted = RemoteResponse::Accepted.unchanged_since(Some(&shown));

    // Assert
    assert_eq!(shown.len(), 32);
    assert_eq!(again, full);
    assert_ne!(version(&other), shown);
    assert_eq!(stale, full);
    assert_eq!(accepted, RemoteResponse::Accepted);
    let encoded = serde_json::to_value(&full).expect("response should encode");
    assert_eq!(encoded["version"], json!(shown));
    assert!(encoded.get("unchanged").is_none());
    assert_eq!(
        serde_json::to_value(&unchanged).expect("response should encode"),
        json!({"kind": "graphs", "unchanged": true, "version": shown})
    );
    let parsed: RemoteResponse = serde_json::from_value(json!({
        "kind": "graphs",
        "unchanged": true,
        "version": shown
    }))
    .expect("response should parse");
    assert_eq!(parsed, unchanged);
}

#[test]
fn graph_frames_carry_the_selected_graph_its_nodes_and_outputs() {
    // Arrange
    let graphs = ag_session::test_support::session_graphs("session-1");

    // Act
    let GraphsParts {
        graph,
        graphs: entries,
        proposal,
    } = graphs_parts(RemoteResponse::from_graphs(graphs));

    // Assert
    assert_eq!(
        entries,
        [RemoteGraphEntry {
            id: "graph".to_string(),
            is_focused_review: false,
            revision: 1,
        }]
    );
    assert_eq!(
        proposal.as_deref(),
        Some(&RemoteGraphProposal {
            graph_id: Some("graph".to_string()),
            hint: None,
            id: "proposal".to_string(),
            phase: RemoteGraphProposalPhase::Approved,
            preview: "## Graph proposal v1".to_string(),
            questions: Vec::new(),
            truncated: false,
            version: 1,
        })
    );
    let graph = graph.expect("a graph should be selected");
    assert_eq!(graph.node.as_deref(), Some("inspect"));
    assert_eq!(graph.status, Some(RemoteGraphStatus::Blocked));
    assert_eq!(graph.status_label, "Blocked");
    assert_eq!(graph.snapshot, "5eed");
    assert_eq!(graph.actions, [RemoteGraphAction::Cancel]);
    assert_eq!(graph.budget, None);
    assert_eq!(
        graph.nodes,
        [
            RemoteGraphNode {
                actions: Vec::new(),
                agent: "inspector".to_string(),
                agent_kind: "codex".to_string(),
                attempts: 1,
                awaits_approval: true,
                blocked: Vec::new(),
                error: None,
                hint: None,
                id: "inspect".to_string(),
                label: "Awaiting your approval".to_string(),
                model: "gpt-6.1-sol".to_string(),
                objective: "Run inspect".to_string(),
                questions: Vec::new(),
                reasoning: "low".to_string(),
                requires: Vec::new(),
                status: RemoteGraphNodeStatus::Succeeded,
                truncated: false,
            },
            RemoteGraphNode {
                actions: Vec::new(),
                agent: "inspector".to_string(),
                agent_kind: "codex".to_string(),
                attempts: 0,
                awaits_approval: false,
                blocked: vec!["inspect: accepted report".to_string()],
                error: None,
                hint: None,
                id: "fix".to_string(),
                label: "Waiting".to_string(),
                model: "gpt-6.1-sol".to_string(),
                objective: "Run fix".to_string(),
                questions: Vec::new(),
                reasoning: "low".to_string(),
                requires: vec![RemoteGraphRequirement {
                    gate: "accepted report".to_string(),
                    label: "needs report accepted".to_string(),
                    node_id: "inspect".to_string(),
                    satisfied: false,
                }],
                status: RemoteGraphNodeStatus::Pending,
                truncated: false,
            },
        ]
    );
    assert_eq!(
        graph.outputs,
        [RemoteGraphOutput {
            actions: vec![RemoteGraphAction::Accept],
            id: "result".to_string(),
            name: "report".to_string(),
            state: RemoteGraphOutputState::Validated,
            text: "### Findings\n- One issue".to_string(),
            truncated: false,
            version: 1,
        }]
    );
}

#[test]
fn graph_frames_encode_states_in_snake_case_and_omit_what_is_empty() {
    // Arrange
    let response =
        RemoteResponse::from_graphs(ag_session::test_support::session_graphs("session-1"));

    // Act
    let encoded = serde_json::to_value(&response).expect("response should encode");

    // Assert
    assert_eq!(encoded["kind"], "graphs");
    assert_eq!(encoded["graph"]["status"], "blocked");
    assert_eq!(encoded["graph"]["nodes"][0]["status"], "succeeded");
    assert_eq!(encoded["graph"]["nodes"][1]["status"], "pending");
    assert_eq!(encoded["graph"]["outputs"][0]["state"], "validated");
    assert!(
        encoded["graph"]["nodes"][1]
            .get("awaits_approval")
            .is_none()
    );
    assert_eq!(encoded["graph"]["actions"], json!(["cancel"]));
    assert_eq!(encoded["graph"]["outputs"][0]["actions"], json!(["accept"]));
    assert!(encoded["graph"].get("budget").is_none());
    assert!(encoded["graph"]["nodes"][0].get("actions").is_none());
    assert!(encoded["proposal"].get("truncated").is_none());
    assert!(encoded["graph"]["outputs"][0].get("truncated").is_none());
    assert!(encoded["graph"]["nodes"][0].get("truncated").is_none());
    assert_eq!(
        serde_json::from_value::<RemoteResponse>(encoded).expect("response should decode"),
        response
    );
}

#[test]
fn graph_nodes_without_progress_read_as_pending_and_questions_carry_over() {
    // Arrange
    let mut graphs = ag_session::test_support::session_graphs("session-1");
    let graph = graphs.graph.as_mut().expect("graph");
    graph.nodes[1].progress = None;
    let asking = graph.nodes[0].progress.as_mut().expect("progress");
    asking.status = GraphNodeStatus::AwaitingAnswers;
    asking.last_error = Some("needs more context".to_string());
    asking.questions = vec![QuestionItem {
        options: vec!["main".to_string()],
        text: "Which branch?".to_string(),
    }];
    graph.nodes[0].hint = Some("reply in chat to answer".to_string());
    graph.status = None;
    let proposal = graphs.proposal.as_mut().expect("proposal");
    proposal.state.version.approval = None;
    proposal.phase = SessionGraphProposalPhase::NeedsInput;
    proposal.hint = Some("reply in chat to approve, revise, or cancel".to_string());
    proposal.questions = vec![QuestionItem {
        options: Vec::new(),
        text: "Which files?".to_string(),
    }];

    // Act
    let GraphsParts {
        graph, proposal, ..
    } = graphs_parts(RemoteResponse::from_graphs(graphs));

    // Assert
    let graph = graph.expect("graph");
    assert_eq!(graph.status, None);
    assert_eq!(
        graph.nodes[0].status,
        RemoteGraphNodeStatus::AwaitingAnswers
    );
    assert_eq!(graph.nodes[0].error.as_deref(), Some("needs more context"));
    assert_eq!(
        graph.nodes[0].hint.as_deref(),
        Some("reply in chat to answer")
    );
    assert_eq!(
        graph.nodes[0].questions,
        [RemoteQuestion {
            options: vec!["main".to_string()],
            text: "Which branch?".to_string(),
        }]
    );
    assert_eq!(graph.nodes[1].status, RemoteGraphNodeStatus::Pending);
    assert_eq!(graph.nodes[1].attempts, 0);
    let proposal = proposal.expect("proposal");
    assert_eq!(proposal.graph_id, None);
    assert_eq!(proposal.phase, RemoteGraphProposalPhase::NeedsInput);
    assert_eq!(
        proposal.questions,
        [RemoteQuestion {
            options: Vec::new(),
            text: "Which files?".to_string(),
        }]
    );
}

#[test]
fn every_graph_state_has_a_snake_case_name() {
    // Arrange
    let statuses = [
        (ExecutionGraphStatus::Blocked, "blocked"),
        (ExecutionGraphStatus::Cancelled, "cancelled"),
        (ExecutionGraphStatus::Completed, "completed"),
        (ExecutionGraphStatus::Failed, "failed"),
        (ExecutionGraphStatus::Paused, "paused"),
        (ExecutionGraphStatus::Running, "running"),
    ];
    let node_statuses = [
        (GraphNodeStatus::AwaitingAnswers, "awaiting_answers"),
        (GraphNodeStatus::Cancelled, "cancelled"),
        (GraphNodeStatus::Failed, "failed"),
        (GraphNodeStatus::Pending, "pending"),
        (GraphNodeStatus::Queued, "queued"),
        (GraphNodeStatus::Running, "running"),
        (GraphNodeStatus::Succeeded, "succeeded"),
    ];
    let phases = [
        (SessionGraphProposalPhase::Approved, "approved"),
        (
            SessionGraphProposalPhase::AwaitingApproval,
            "awaiting_approval",
        ),
        (SessionGraphProposalPhase::Cancelled, "cancelled"),
        (SessionGraphProposalPhase::Failed, "failed"),
        (SessionGraphProposalPhase::Generating, "generating"),
        (SessionGraphProposalPhase::NeedsInput, "needs_input"),
        (SessionGraphProposalPhase::Unresolved, "unresolved"),
    ];
    let output_states = [
        (ag_session::GraphResultState::Accepted, "accepted"),
        (ag_session::GraphResultState::Produced, "produced"),
        (ag_session::GraphResultState::Validated, "validated"),
    ];

    // Act / Assert
    for (status, name) in statuses {
        assert_eq!(json!(RemoteGraphStatus::from(status)), json!(name));
    }
    for (status, name) in node_statuses {
        assert_eq!(json!(RemoteGraphNodeStatus::from(status)), json!(name));
    }
    for (phase, name) in phases {
        assert_eq!(json!(RemoteGraphProposalPhase::from(phase)), json!(name));
    }
    for (state, name) in output_states {
        assert_eq!(json!(RemoteGraphOutputState::from(state)), json!(name));
    }
}

/// Returns lines of 1023 `x` characters that together exceed
/// [`MAX_FILE_RESPONSE_BYTES`].
fn oversized_text() -> String {
    format!("{}\n", "x".repeat(1023)).repeat(MAX_FILE_RESPONSE_BYTES / 1024 + 4)
}

#[test]
fn a_large_preview_is_cut_and_marked_on_its_own() {
    // Arrange
    let mut graphs = ag_session::test_support::session_graphs("session-1");
    graphs.proposal.as_mut().expect("proposal").preview = oversized_text();

    // Act
    let GraphsParts {
        graph, proposal, ..
    } = graphs_parts(RemoteResponse::from_graphs(graphs));

    // Assert
    let proposal = proposal.expect("proposal");
    assert!(proposal.truncated);
    assert!(proposal.preview.len() <= MAX_FILE_RESPONSE_BYTES);
    assert!(proposal.preview.ends_with('\n'));
    // The complete outputs say nothing was cut.
    assert!(!graph.expect("graph").outputs[0].truncated);
}

#[test]
fn a_large_output_is_cut_and_marked_while_the_small_preview_is_whole() {
    // Arrange
    let mut graphs = ag_session::test_support::session_graphs("session-1");
    let graph = graphs.graph.as_mut().expect("graph");
    graph.outputs[0].display = oversized_text();

    // Act
    let GraphsParts {
        graph, proposal, ..
    } = graphs_parts(RemoteResponse::from_graphs(graphs));

    // Assert
    assert!(!proposal.expect("proposal").truncated);
    let output = &graph.expect("graph").outputs[0];
    assert!(output.truncated);
    assert!(output.text.len() <= MAX_FILE_RESPONSE_BYTES);
    assert!(output.text.ends_with('\n'));
}

#[test]
fn an_output_left_without_room_is_marked_instead_of_empty() {
    // Arrange: the first output takes the whole budget, the second fits
    // into what is left only in part, and the third finds none.
    let mut graphs = ag_session::test_support::session_graphs("session-1");
    let graph = graphs.graph.as_mut().expect("graph");
    graph.outputs[0].display = format!("{}\n", "x".repeat(MAX_FILE_RESPONSE_BYTES - 400));
    let mut second = graph.outputs[0].clone();
    second.display = "y".repeat(1000);
    let mut third = second.clone();
    third.display = "z".repeat(10);
    graph.outputs.extend([second, third]);

    // Act
    let outputs = graphs_parts(RemoteResponse::from_graphs(graphs))
        .graph
        .expect("graph")
        .outputs;

    // Assert
    assert_eq!(
        outputs
            .iter()
            .map(|output| (output.text.len(), output.truncated))
            .collect::<Vec<_>>(),
        [
            (MAX_FILE_RESPONSE_BYTES - 399, false),
            (398, true),
            (0, true)
        ]
    );
}

#[test]
fn long_node_errors_and_questions_are_cut_within_their_shared_budget() {
    // Arrange
    let mut graphs = ag_session::test_support::session_graphs("session-1");
    let graph = graphs.graph.as_mut().expect("graph");
    let failing = graph.nodes[0].progress.as_mut().expect("progress");
    failing.last_error = Some("e".repeat(MAX_FILE_RESPONSE_BYTES));
    let asking = graph.nodes[1].progress.as_mut().expect("progress");
    asking.questions = vec![QuestionItem {
        options: vec!["main".to_string()],
        text: "Which branch?".to_string(),
    }];

    // Act
    let nodes = graphs_parts(RemoteResponse::from_graphs(graphs))
        .graph
        .expect("graph")
        .nodes;

    // Assert
    // The first node's objective, `Run inspect`, comes first.
    assert!(nodes[0].truncated);
    assert_eq!(
        nodes[0].error.as_deref().map(str::len),
        Some(MAX_FILE_RESPONSE_BYTES - "Run inspect".len())
    );
    // The error took the rest of the budget, so what follows is cut too.
    assert!(nodes[1].truncated);
    assert_eq!(nodes[1].objective, "");
    assert_eq!(
        nodes[1].questions,
        [RemoteQuestion {
            options: vec![String::new()],
            text: String::new(),
        }]
    );
}

#[test]
fn graph_frames_from_an_older_or_empty_host_parse_without_details() {
    // Arrange
    let wire = json!({"kind": "graphs"});

    // Act
    let parsed: RemoteResponse = serde_json::from_value(wire).expect("response should parse");
    let empty = RemoteResponse::from_graphs(SessionGraphs {
        graph: None,
        graphs: Vec::new(),
        proposal: None,
    });

    // Assert
    assert_eq!(
        parsed,
        RemoteResponse::Graphs {
            graph: None,
            graphs: Vec::new(),
            proposal: None,
            unchanged: false,
            version: String::new(),
        }
    );
    let encoded = serde_json::to_value(&empty).expect("response should encode");
    assert_eq!(encoded.as_object().map(serde_json::Map::len), Some(2));
    assert_eq!(encoded["kind"], json!("graphs"));
    assert!(encoded["version"].is_string());
}

#[test]
fn graph_frames_list_every_action_where_it_applies_and_the_spent_budget() {
    // Arrange
    let mut graphs = ag_session::test_support::session_graphs("session-1");
    let graph = graphs.graph.as_mut().expect("graph");
    graph.actions = vec![
        SessionGraphActionKind::Cancel,
        SessionGraphActionKind::ExtendBudget,
        SessionGraphActionKind::Resume,
    ];
    graph.exhausted_budget = Some("calls".to_string());
    graph.nodes[0].actions = vec![SessionGraphActionKind::Reply, SessionGraphActionKind::Retry];

    // Act
    let response = RemoteResponse::from_graphs(graphs);
    let encoded = serde_json::to_value(&response).expect("response should encode");

    // Assert
    assert_eq!(
        serde_json::from_value::<RemoteResponse>(encoded.clone()).expect("response should decode"),
        response
    );
    assert_eq!(
        encoded["graph"]["actions"],
        json!(["cancel", "extend_budget", "resume"])
    );
    assert_eq!(
        encoded["graph"]["budget"],
        json!({
            "id": "calls",
            "max_additional_calls": ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS
        })
    );
    assert_eq!(
        encoded["graph"]["nodes"][0]["actions"],
        json!(["reply", "retry"])
    );
}

/// An `act_on_session_graph` frame of `graph` in session `s` bound to the
/// snapshot `5eed`, with `fields` added.
fn graph_action_frame(fields: &serde_json::Value) -> serde_json::Value {
    let mut frame = json!({
        "method": "act_on_session_graph",
        "graph_id": "graph",
        "session_id": "s",
        "snapshot": "5eed"
    });
    for (name, value) in fields.as_object().expect("fields should be an object") {
        frame[name] = value.clone();
    }

    frame
}

/// Parses `frame` and converts it to the session API request.
fn parse_graph_action(frame: serde_json::Value) -> Result<SessionGraphActionRequest, SessionError> {
    let RemoteRequest::ActOnSessionGraph(request) =
        serde_json::from_value(frame).expect("request should parse")
    else {
        return Err(SessionError::NotFound);
    };

    request.into_session_request(SessionId::from("s"))
}

#[test]
fn graph_action_requests_carry_the_fields_each_action_needs() {
    // Arrange
    let cases = [
        (
            json!({"action": "accept", "node_id": "inspect", "output_id": "result"}),
            SessionGraphAction::Accept {
                node_id: "inspect".to_string(),
                output_id: "result".to_string(),
            },
        ),
        (json!({"action": "cancel"}), SessionGraphAction::Cancel),
        (
            json!({"action": "extend_budget", "additional_calls": 5, "budget_id": "calls"}),
            SessionGraphAction::ExtendBudget {
                additional_calls: 5,
                budget_id: "calls".to_string(),
            },
        ),
        (
            json!({"action": "reply", "message": "Shorter", "node_id": "fix"}),
            SessionGraphAction::Reply {
                message: "Shorter".to_string(),
                node_id: "fix".to_string(),
            },
        ),
        (json!({"action": "resume"}), SessionGraphAction::Resume),
        (
            json!({"action": "retry", "node_id": "fix"}),
            SessionGraphAction::Retry {
                node_id: "fix".to_string(),
            },
        ),
    ];

    for (fields, expected) in cases {
        // Act
        let frame = graph_action_frame(&fields);
        let request = parse_graph_action(frame.clone());
        let reencoded = serde_json::to_value(
            serde_json::from_value::<RemoteRequest>(frame.clone()).expect("request should parse"),
        )
        .expect("request should encode");

        // Assert
        assert_eq!(
            request,
            Ok(SessionGraphActionRequest {
                action: expected,
                graph_id: "graph".to_string(),
                session_id: SessionId::from("s"),
                snapshot: "5eed".to_string(),
            }),
            "{fields}"
        );
        assert_eq!(reencoded, frame, "{fields}");
    }
}

#[test]
fn graph_action_requests_without_a_needed_field_or_with_an_unknown_action_are_refused() {
    // Arrange
    let cases = [
        (
            json!({"action": "accept", "output_id": "result"}),
            "node_id",
        ),
        (
            json!({"action": "accept", "node_id": "inspect"}),
            "output_id",
        ),
        (
            json!({"action": "extend_budget", "budget_id": "calls"}),
            "additional_calls",
        ),
        (
            json!({"action": "extend_budget", "additional_calls": 5}),
            "budget_id",
        ),
        (json!({"action": "reply", "node_id": "fix"}), "message"),
        (json!({"action": "reply", "message": "Shorter"}), "node_id"),
        (json!({"action": "retry"}), "node_id"),
    ];

    for (fields, field) in cases {
        // Act
        let request = parse_graph_action(graph_action_frame(&fields));

        // Assert
        assert_eq!(
            request,
            Err(SessionError::Operation(format!(
                "Graph action needs `{field}`"
            ))),
            "{fields}"
        );
    }
    assert_eq!(
        parse_graph_action(graph_action_frame(&json!({"action": "rewrite_history"}))),
        Err(SessionError::Operation(
            "Unsupported graph action".to_string()
        ))
    );
}

#[test]
fn graph_proposal_requests_carry_the_task_and_its_session() {
    // Arrange
    let frame = json!({
        "method": "generate_graph_proposal",
        "message": "Split the parser",
        "session_id": "s"
    });

    // Act
    let request: RemoteRequest =
        serde_json::from_value(frame.clone()).expect("request should parse");
    let encoded = serde_json::to_value(&request).expect("request should encode");

    // Assert
    assert_eq!(
        request,
        RemoteRequest::GenerateGraphProposal {
            message: "Split the parser".to_string(),
            session_id: "s".to_string(),
        }
    );
    assert_eq!(encoded, frame);
}

#[test]
fn answers_name_the_graph_work_they_answer_and_omit_it_for_the_turn() {
    // Arrange
    let targeted = json!({
        "method": "answer_questions",
        "answers": [{"answer": "Local", "question": "Where?"}],
        "graph_question_target": "c0ffee",
        "session_id": "s"
    });
    let older = json!({
        "method": "answer_questions",
        "answers": [{"answer": "main", "question": "Which branch?"}],
        "session_id": "s"
    });

    // Act
    let parsed: RemoteRequest =
        serde_json::from_value(targeted.clone()).expect("request should parse");
    let parsed_older: RemoteRequest =
        serde_json::from_value(older.clone()).expect("request should parse");

    // Assert
    assert!(matches!(
        &parsed,
        RemoteRequest::AnswerQuestions { graph_question_target: Some(target), .. } if target == "c0ffee"
    ));
    assert!(matches!(
        &parsed_older,
        RemoteRequest::AnswerQuestions {
            graph_question_target: None,
            ..
        }
    ));
    assert_eq!(
        serde_json::to_value(&parsed).expect("request should encode"),
        targeted
    );
    assert_eq!(
        serde_json::to_value(&parsed_older).expect("request should encode"),
        older
    );
}

#[test]
fn graph_questions_travel_apart_from_the_questions_of_the_turn() {
    // Arrange
    let mut session = session_fixture(&SessionId::from("session-1"));
    session.questions = vec![QuestionItem {
        options: vec!["Local".to_string(), "Remote".to_string()],
        text: "Where should it run?".to_string(),
    }];
    session.graph_question_target = Some("c0ffee".to_string());
    let turn = session_fixture(&SessionId::from("session-1"));

    // Act
    let remote = RemoteSession::from(session);
    let encoded = serde_json::to_value(&remote).expect("session should encode");
    let turn_encoded =
        serde_json::to_value(RemoteSession::from(turn)).expect("session should encode");

    // Assert
    assert_eq!(remote.questions, Vec::new());
    assert_eq!(
        remote.graph_questions,
        [RemoteQuestion {
            options: vec!["Local".to_string(), "Remote".to_string()],
            text: "Where should it run?".to_string(),
        }]
    );
    assert_eq!(encoded["questions"], json!([]));
    assert_eq!(
        encoded["graph_questions"],
        json!([{"options": ["Local", "Remote"], "text": "Where should it run?"}])
    );
    assert_eq!(encoded["graph_question_target"], json!("c0ffee"));
    assert!(turn_encoded.get("graph_questions").is_none());
    assert!(turn_encoded.get("graph_question_target").is_none());
    assert_eq!(turn_encoded["questions"][0]["text"], json!("Which branch?"));
}
