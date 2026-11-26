//! Named model fixtures shared by session, transport, persistence, and host
//! tests.
//!
//! These selections are pinned independently of provider defaults. Update them
//! only when their model is retired; tests of actual defaults and model ids
//! should keep explicit expectations instead.

use crate::{AgentDefinition, AgentKind, AgentModel, AgentSelection};

/// Builds a minimal definition for tests unrelated to bundled prompt content.
#[must_use]
pub fn agent_definition(id: &str) -> AgentDefinition {
    AgentDefinition {
        description: "Test agent".to_string(),
        id: id.to_string(),
        name: id.to_string(),
        prompt: "Follow the supplied task input within session permissions.".to_string(),
        read_only: false,
    }
}

/// Supported Codex model used by tests unrelated to a particular model version.
pub const CODEX_MODEL: AgentModel = AgentModel::Gpt61Sol;

/// Wire id of the pinned Codex fixture, including for persisted demo sessions.
pub const CODEX_MODEL_ID: &str = CODEX_MODEL.as_str();

/// Builds an explicit Codex selection without consulting the provider default.
#[must_use]
pub fn codex_selection() -> AgentSelection {
    AgentSelection::new(AgentKind::Codex, CODEX_MODEL)
}

/// Minimal utility proposal shared by resolution, persistence, and host tests.
#[must_use]
pub fn graph_proposal() -> ag_protocol::GraphProposal {
    ag_protocol::GraphProposal {
        nodes: vec![ag_protocol::GraphProposalNode {
            agent_id: "inspector".into(),
            dependencies: Vec::new(),
            execution: ag_protocol::GraphProposalExecution {
                context: ag_protocol::GraphProposalContext::Fresh,
                post_processing: ag_protocol::GraphProposalPostProcessing::None,
                response_profile: ag_protocol::ProtocolRequestProfile::UtilityPrompt,
                workspace_access: ag_protocol::GraphProposalWorkspaceAccess::SharedRead,
            },
            id: "inspect".into(),
            inputs: Vec::new(),
            objective: "Inspect the requested change".into(),
            outputs: vec![ag_protocol::GraphProposalOutput {
                name: "report".into(),
                schema: ag_protocol::ProtocolRequestProfile::UtilityPrompt,
            }],
        }],
    }
}

/// Captured host inputs for a first, durably reserved proposal generation.
#[must_use]
pub fn graph_proposal_state(id: &str, session_id: &str) -> crate::GraphProposalState {
    crate::GraphProposalState {
        agent: codex_selection(),
        base: None,
        definitions: vec![agent_definition("inspector")],
        input: crate::GenerateGraphProposalRequest {
            id: id.into(),
            request: ag_protocol::TurnPrompt::from_text("Inspect the requested change".into()),
            session_id: session_id.into(),
        },
        limits: crate::GraphProposalLimits::default(),
        permission: crate::PermissionMode::AutoEdit,
        version: crate::GraphProposalVersion {
            answers: Vec::new(),
            approval: None,
            attachments: Vec::new(),
            is_cancelled: false,
            number: 1,
            outcome: crate::GraphProposalVersionOutcome::Generating,
            parent_id: format!("graph-proposal:{id}:1"),
            utility_run_id: None,
        },
    }
}

/// Builds a fresh utility node `id` of `session_id` that reports `report`.
#[must_use]
pub fn graph_node(id: &str, session_id: &str) -> crate::GraphNode {
    crate::GraphNode {
        agent: codex_selection(),
        definition: agent_definition("inspector"),
        dependencies: Vec::new(),
        env_id: crate::EnvId::SessionWorktree(session_id.into()),
        execution: crate::GraphExecutionProfile {
            context: crate::GraphContext::Fresh,
            post_processing: crate::GraphPostProcessing::None,
            response_profile: ag_protocol::ProtocolRequestProfile::UtilityPrompt,
            shared_budget_id: None,
            workspace_access: crate::GraphWorkspaceAccess::SharedRead,
        },
        id: id.into(),
        inputs: Vec::new(),
        objective: format!("Run {id}"),
        outputs: vec![crate::GraphOutput {
            name: "report".into(),
            schema: ag_protocol::ProtocolRequestProfile::UtilityPrompt,
        }],
        request: ag_protocol::TurnPrompt::from_agent_data(format!("Run {id}")),
    }
}

/// Builds what a host reports for `session_id` while its approved graph
/// `graph` waits for the user: `inspect` succeeded and its validated
/// `report` waits for acceptance, which `fix` needs before it can run. The
/// graph offers cancellation and the output acceptance, as a host's
/// inspector would.
#[must_use]
pub fn session_graphs(session_id: &str) -> crate::SessionGraphs {
    let revision = std::num::NonZeroU64::MIN;
    let mut proposal = graph_proposal_state("proposal", session_id);
    proposal.version.approval = Some(crate::GraphProposalApproval {
        graph_id: "graph".into(),
        limits: proposal.limits.clone(),
        version: 1,
    });
    let execution = crate::GraphExecutionRef::UtilityRun {
        run_id: "run".into(),
    };
    let report = crate::GraphResult {
        reference: crate::GraphResultRef {
            execution: execution.clone(),
            graph_id: "graph".into(),
            graph_revision: revision,
            id: "result".into(),
            node_id: "inspect".into(),
            output_name: "report".into(),
            schema: ag_protocol::ProtocolRequestProfile::UtilityPrompt,
            version: revision,
        },
        state: crate::GraphResultState::Validated,
    };

    crate::SessionGraphs {
        graph: Some(crate::SessionGraph {
            actions: vec![crate::SessionGraphActionKind::Cancel],
            exhausted_budget: None,
            id: "graph".into(),
            is_focused_review: false,
            node_id: Some("inspect".into()),
            nodes: vec![
                crate::SessionGraphNode {
                    actions: Vec::new(),
                    awaits_approval: true,
                    blocked: Vec::new(),
                    hint: None,
                    node: graph_node("inspect", session_id),
                    progress: Some(crate::GraphNodeProgress {
                        blocked: Vec::new(),
                        executions: vec![execution],
                        last_error: None,
                        node_id: "inspect".into(),
                        questions: Vec::new(),
                        results: vec![report.clone()],
                        status: crate::GraphNodeStatus::Succeeded,
                    }),
                    reasoning_level: crate::ReasoningLevel::Low,
                    requirements: Vec::new(),
                    status_label: "Awaiting your approval".into(),
                },
                waiting_fix_node(session_id),
            ],
            note: None,
            outputs: vec![crate::SessionGraphOutput {
                actions: vec![crate::SessionGraphActionKind::Accept],
                body: "{\"answer\":\"### Findings\"}".into(),
                display: "### Findings\n- One issue".into(),
                result: report,
            }],
            revision,
            snapshot: "5eed".into(),
            status: Some(crate::ExecutionGraphStatus::Blocked),
            status_label: "Blocked".into(),
        }),
        graphs: vec![crate::SessionGraphEntry {
            id: "graph".into(),
            is_focused_review: false,
            revision,
        }],
        proposal: Some(crate::SessionGraphProposal {
            hint: None,
            phase: crate::SessionGraphProposalPhase::Approved,
            preview: "## Graph proposal v1".into(),
            questions: Vec::new(),
            state: proposal,
        }),
    }
}

/// The `fix` node of [`session_graphs`], waiting for `inspect`'s `report`
/// to be accepted.
fn waiting_fix_node(session_id: &str) -> crate::SessionGraphNode {
    let gate = crate::GraphCompletionGate::ResultAccepted {
        output_name: "report".into(),
    };
    let mut fix = graph_node("fix", session_id);
    fix.dependencies.push(crate::GraphDependency {
        gate: gate.clone(),
        node_id: "inspect".into(),
    });

    crate::SessionGraphNode {
        actions: Vec::new(),
        awaits_approval: false,
        blocked: vec!["inspect: accepted report".into()],
        hint: None,
        node: fix,
        progress: Some(crate::GraphNodeProgress {
            blocked: vec![crate::GraphBlockedReason::Dependency {
                gate,
                node_id: "inspect".into(),
            }],
            executions: Vec::new(),
            last_error: None,
            node_id: "fix".into(),
            questions: Vec::new(),
            results: Vec::new(),
            status: crate::GraphNodeStatus::Pending,
        }),
        reasoning_level: crate::ReasoningLevel::Low,
        requirements: vec![crate::SessionGraphRequirement {
            gate: "accepted report".into(),
            is_satisfied: false,
            label: "needs report accepted".into(),
            node_id: "inspect".into(),
        }],
        status_label: "Waiting".into(),
    }
}

#[test]
fn codex_fixture_uses_an_explicit_supported_selection() {
    // Arrange
    let expected_id = "gpt-6.1-sol";

    // Act
    let selection = codex_selection();

    // Assert
    assert_eq!(selection.kind(), AgentKind::Codex);
    assert_eq!(selection.model(), CODEX_MODEL);
    assert_eq!(CODEX_MODEL_ID, expected_id);
    assert_eq!(selection.model().as_str(), expected_id);
    assert!(selection.supports_fast_mode());
}
