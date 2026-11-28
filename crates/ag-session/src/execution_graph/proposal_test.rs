//! Public resolution contract: model output is declarative and grants no
//! authority.

use ag_protocol::{
    GraphProposalContext, GraphProposalDependency, GraphProposalGate, GraphProposalInput,
    GraphProposalPostProcessing, GraphProposalWorkspaceAccess, ProtocolRequestProfile,
    TurnPromptAttachment,
};

use crate::test_support::{graph_node, graph_proposal, graph_proposal_state};
use crate::{
    EnvId, GraphContext, GraphNode, GraphProposalResolution, GraphProposalState, PermissionMode,
    TaskWorktreeId,
};

#[test]
fn graph_proposal_state_persists_its_permission_ceiling_as_a_stable_label() {
    // Arrange
    let mut state = graph_proposal_state("proposal", "session");
    state.permission = PermissionMode::ReadOnly;

    // Act
    let mut value = serde_json::to_value(&state).expect("state should serialize");
    let restored =
        serde_json::from_value::<GraphProposalState>(value.clone()).expect("state should load");
    value["permission"] = serde_json::json!("unrestricted");
    let invalid = serde_json::from_value::<GraphProposalState>(value);

    // Assert
    assert_eq!(restored, state);
    assert!(invalid.is_err());
}

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
            revision_attachments: &state.version.attachments,
            session_id: &state.input.session_id,
        }
        .resolve(&proposal);

        // Assert
        if [
            "valid",
            "task",
            "effects",
            "input",
            "accepted",
            "attachments",
        ]
        .contains(&case)
        {
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
fn graph_proposal_resolution_carries_complete_history_and_images_to_every_node() {
    // Arrange
    let mut state = graph_proposal_state("preview", "session");
    let request_image = TurnPromptAttachment {
        local_image_path: "request.png".into(),
        placeholder: "[Image #1]".into(),
    };
    let revision_image = TurnPromptAttachment {
        local_image_path: "revision.png".into(),
        placeholder: "[Image #1]".into(),
    };
    state.input.request.attachments.push(request_image.clone());
    state.version.attachments.push(revision_image.clone());
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
        revision_attachments: &state.version.attachments,
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
            node.request.attachments,
            [request_image.clone(), revision_image.clone()]
        );
        assert_eq!(
            serde_json::from_str::<Vec<String>>(history).expect("history"),
            state.version.answers
        );
        assert_eq!(node.objective, proposal.nodes[0].objective);
    }
}

#[test]
fn graph_expansion_resolution_appends_nodes_after_the_base_tasks() {
    // Arrange
    let state = graph_proposal_state("preview", "session");
    let resolution = GraphProposalResolution {
        agent: state.agent,
        answers: &state.version.answers,
        definitions: &state.definitions,
        graph_id: "host-graph",
        limits: &state.limits,
        permission: state.permission,
        request: &state.input.request,
        revision_attachments: &state.version.attachments,
        session_id: &state.input.session_id,
    };
    let mut task_proposal = graph_proposal();
    configure_case("task", &mut state.clone(), &mut task_proposal);
    let base = resolution.resolve(&task_proposal).expect("base");
    let mut added = graph_proposal();
    configure_case("task", &mut state.clone(), &mut added);
    added.nodes[0].id = "follow-up".into();
    added.nodes[0].dependencies.push(GraphProposalDependency {
        gate: GraphProposalGate::ExecutionSucceeded {},
        node_id: "inspect".into(),
    });
    let mut foreign = base.clone();
    foreign.id = "other-graph".into();

    // Act
    let expanded = resolution.resolve_expansion(&base, &added);
    let reused = resolution.resolve_expansion(&base, &task_proposal);
    let mismatched = resolution.resolve_expansion(&foreign, &added);

    // Assert
    let expanded = expanded.expect("expanded");
    assert_eq!(expanded.revision.get(), 2);
    assert_eq!(expanded.nodes[0], base.nodes[0]);
    assert_eq!(
        expanded.nodes[1].execution.context,
        GraphContext::TaskConversation {
            task_id: "host-graph:task:1".into()
        }
    );
    assert_eq!(
        expanded.nodes[1].execution.shared_budget_id.as_deref(),
        Some("host-graph:calls")
    );
    assert!(reused.is_err());
    assert_eq!(
        mismatched,
        Err("Graph expansion targets a different graph or session".into())
    );
}

#[test]
fn graph_proposal_resolution_binds_children_to_their_parent_env() {
    // Arrange
    let state = graph_proposal_state("preview", "session");
    let resolution = GraphProposalResolution {
        agent: state.agent,
        answers: &state.version.answers,
        definitions: &state.definitions,
        graph_id: "host-graph",
        limits: &state.limits,
        permission: state.permission,
        request: &state.input.request,
        revision_attachments: &state.version.attachments,
        session_id: &state.input.session_id,
    };
    let mut proposal = graph_proposal();
    let mut child = proposal.nodes[0].clone();
    child.id = "tests".into();
    child.parent_id = Some("inspect".into());
    proposal.nodes.insert(0, child.clone());
    let base = resolution.resolve(&graph_proposal()).expect("base");
    child.id = "docs".into();
    let added = ag_protocol::GraphProposal {
        nodes: vec![child.clone()],
    };
    let mut nested = child.clone();
    nested.parent_id = Some("tests".into());
    let mut orphan = child;
    orphan.parent_id = Some("missing".into());

    // Act
    let graph = resolution.resolve(&proposal).expect("resolved");
    let expanded = resolution
        .resolve_expansion(&base, &added)
        .expect("child of an existing node");
    let nested = resolution.resolve(&ag_protocol::GraphProposal {
        nodes: vec![nested, proposal.nodes[0].clone(), proposal.nodes[1].clone()],
    });
    let orphan = resolution.resolve(&ag_protocol::GraphProposal {
        nodes: vec![orphan],
    });

    // Assert
    assert_eq!(graph.nodes[0].parent_id.as_deref(), Some("inspect"));
    assert_eq!(graph.nodes[0].env_id, graph.nodes[1].env_id);
    assert_eq!(graph.nodes[1].parent_id, None);
    assert_eq!(expanded.nodes[1].parent_id.as_deref(), Some("inspect"));
    assert_eq!(expanded.nodes[1].env_id, base.nodes[0].env_id);
    assert_eq!(
        nested,
        Err(
            "graph node `docs` cannot be a child of `tests`, which is itself a child; children \
             cannot have children, so name a top-level node as its parent"
                .into()
        )
    );
    assert_eq!(
        orphan,
        Err(
            "graph node `docs` names parent `missing`, which is not a node of this graph; name a \
             top-level node of this graph as its parent, or give it no parent"
                .into()
        )
    );
}

#[test]
fn children_take_their_parent_env_from_the_base_or_the_proposal() {
    // Arrange: the base parent works in a task worktree, which proposals
    // cannot name, so only the binding can give the child that Env.
    let worktree = EnvId::TaskWorktree(TaskWorktreeId {
        session_id: "session".into(),
        worktree_id: "writer".into(),
    });
    let base = [GraphNode {
        env_id: worktree.clone(),
        ..graph_node("change", "session")
    }];
    let child = |id: &str, parent: &str| GraphNode {
        parent_id: Some(parent.into()),
        ..graph_node(id, "session")
    };
    let mut nodes = [
        child("tests", "change"),
        child("notes", "summary"),
        graph_node("summary", "session"),
        child("orphan", "missing"),
    ];

    // Act
    GraphProposalResolution::bind_child_envs(&mut nodes, &base);

    // Assert
    let session = EnvId::SessionWorktree("session".into());
    assert_eq!(
        nodes.map(|node| node.env_id),
        [worktree, session.clone(), session.clone(), session]
    );
}

#[test]
fn a_blank_proposed_parent_means_a_top_level_node() {
    // Arrange
    let state = graph_proposal_state("preview", "session");
    let mut proposal = graph_proposal();
    proposal.nodes[0].parent_id = Some(" ".into());

    // Act
    let graph = GraphProposalResolution {
        agent: state.agent,
        answers: &state.version.answers,
        definitions: &state.definitions,
        graph_id: "host-graph",
        limits: &state.limits,
        permission: state.permission,
        request: &state.input.request,
        revision_attachments: &state.version.attachments,
        session_id: &state.input.session_id,
    }
    .resolve(&proposal)
    .expect("blank parent resolves as top-level");

    // Assert
    assert_eq!(graph.nodes[0].parent_id, None);
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
