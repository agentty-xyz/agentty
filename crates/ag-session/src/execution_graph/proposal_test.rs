//! Public resolution contract: model output is declarative and grants no
//! authority.

use ag_protocol::{
    GraphProposalContext, GraphProposalDependency, GraphProposalGate, GraphProposalInput,
    GraphProposalPostProcessing, GraphProposalWorkspaceAccess, ProtocolRequestProfile,
    TurnPromptAttachment,
};

use crate::test_support::{graph_proposal, graph_proposal_state};
use crate::{GraphContext, GraphProposalResolution, GraphProposalState, PermissionMode};

#[test]
fn graph_proposal_resolution_captures_host_authority_and_rejects_invalid_requirements() {
    for case in [
        "valid",
        "task",
        "effects",
        "background",
        "unknown-agent",
        "read-only-agent",
        "read-only-session",
        "size",
        "nodes",
        "edges",
        "calls",
        "generations",
        "request",
        "attachments",
        "duplicate",
        "cycle",
        "missing-node",
        "missing-output",
        "ungated",
        "input",
        "accepted",
        "nested",
        "task-schema",
        "fresh-effects",
    ] {
        // Arrange
        let mut state = graph_proposal_state("preview", "session");
        let mut proposal = graph_proposal();
        configure_case(case, &mut state, &mut proposal);

        // Act
        let result = GraphProposalResolution {
            agent: state.agent,
            answers: &state.version.answers,
            definitions: &state.definitions,
            graph_id: "host-graph",
            limits: &state.limits,
            permission: state.permission,
            request: &state.input.request,
            session_id: &state.input.session_id,
        }
        .resolve(&proposal);

        // Assert
        if ["valid", "task", "effects", "input", "accepted"].contains(&case) {
            let graph = result.expect(case);
            assert_eq!(graph.nodes[0].definition, state.definitions[0]);
            assert_eq!(graph.nodes[0].agent, state.agent);
            assert_eq!(graph.nodes[0].request, state.input.request);
            assert_eq!(
                graph.nodes[0].execution.shared_budget_id.as_deref(),
                Some("host-graph:calls")
            );
            if case == "task" {
                assert_eq!(
                    graph.nodes[0].execution.context,
                    GraphContext::TaskConversation {
                        task_id: "host-graph:task:0".into()
                    }
                );
            }
        } else {
            assert!(result.is_err(), "{case}");
        }
    }
}

#[test]
fn graph_proposal_resolution_carries_complete_clarification_history_to_every_node() {
    // Arrange
    let mut state = graph_proposal_state("preview", "session");
    state.version.answers = vec![
        serde_json::json!({"question": "Target?", "answer": "Local  only"}).to_string(),
        serde_json::json!({"question": "Format?", "answer": "Use \"quotes\"\nand newlines"})
            .to_string(),
    ];
    let mut proposal = graph_proposal();
    let mut other = proposal.nodes[0].clone();
    other.id = "other".into();
    proposal.nodes.push(other);

    // Act
    let graph = GraphProposalResolution {
        agent: state.agent,
        answers: &state.version.answers,
        definitions: &state.definitions,
        graph_id: "host-graph",
        limits: &state.limits,
        permission: state.permission,
        request: &state.input.request,
        session_id: &state.input.session_id,
    }
    .resolve(&proposal)
    .expect("resolved");

    // Assert
    for node in graph.nodes {
        let (request, history) = node
            .request
            .text
            .split_once("\n\nComplete clarification answer history (JSON data):\n")
            .expect("clarification context");
        assert_eq!(request, state.input.request.text);
        assert_eq!(node.request.text_source, state.input.request.text_source);
        assert_eq!(
            serde_json::from_str::<Vec<String>>(history).expect("history"),
            state.version.answers
        );
        assert_eq!(node.objective, proposal.nodes[0].objective);
    }
}

fn configure_case(
    case: &str,
    state: &mut GraphProposalState,
    proposal: &mut ag_protocol::GraphProposal,
) {
    match case {
        "task" | "effects" | "task-schema" | "read-only-agent" | "read-only-session" => {
            let node = &mut proposal.nodes[0];
            node.execution.context = GraphProposalContext::TaskConversation;
            node.execution.response_profile = ProtocolRequestProfile::SessionTurn;
            node.outputs[0].schema = ProtocolRequestProfile::SessionTurn;
            node.execution.workspace_access = GraphProposalWorkspaceAccess::ExclusiveWrite;
            if case == "effects" {
                node.execution.post_processing = GraphProposalPostProcessing::SessionWorkflow;
            }
            if case == "task-schema" {
                node.execution.response_profile = ProtocolRequestProfile::UtilityPrompt;
            }
            if case == "read-only-agent" {
                state.definitions[0].read_only = true;
            }
            if case == "read-only-session" {
                state.permission = PermissionMode::ReadOnly;
            }
        }
        "background" => {
            proposal.nodes[0].execution.workspace_access =
                GraphProposalWorkspaceAccess::BackgroundRead;
        }
        "unknown-agent" => proposal.nodes[0].agent_id = "missing".into(),
        "size" => state.limits.bytes = 1,
        "nodes" => state.limits.nodes = 0,
        "calls" => state.limits.provider_calls = 0,
        "generations" => state.limits.generations = 0,
        "request" => state.input.request.text.clear(),
        "attachments" => state.input.request.attachments.push(TurnPromptAttachment {
            local_image_path: "image.png".into(),
            placeholder: "[Image #1]".into(),
        }),
        "duplicate" => proposal.nodes.push(proposal.nodes[0].clone()),
        "cycle" | "missing-node" | "edges" => {
            proposal.nodes[0]
                .dependencies
                .push(GraphProposalDependency {
                    gate: GraphProposalGate::ExecutionSucceeded {},
                    node_id: if case == "missing-node" {
                        "missing"
                    } else {
                        "inspect"
                    }
                    .into(),
                });
            if case == "edges" {
                state.limits.edges = 0;
            }
        }
        "missing-output" | "ungated" | "input" | "accepted" => {
            let mut consumer = proposal.nodes[0].clone();
            consumer.id = "join".into();
            consumer.inputs.push(GraphProposalInput {
                name: "evidence".into(),
                output_name: "report".into(),
                producer_node_id: "inspect".into(),
            });
            if case != "ungated" {
                consumer.dependencies.push(GraphProposalDependency {
                    node_id: "inspect".into(),
                    gate: if case == "accepted" {
                        GraphProposalGate::ResultAccepted {
                            output_name: "report".into(),
                        }
                    } else {
                        GraphProposalGate::ResultValidated {
                            output_name: if case == "missing-output" {
                                "missing"
                            } else {
                                "report"
                            }
                            .into(),
                        }
                    },
                });
            }
            proposal.nodes.push(consumer);
        }
        "nested" => {
            proposal.nodes[0].execution.response_profile = ProtocolRequestProfile::GraphProposal;
        }
        "fresh-effects" => {
            proposal.nodes[0].execution.post_processing =
                GraphProposalPostProcessing::SessionWorkflow;
        }
        _ => {}
    }
}
