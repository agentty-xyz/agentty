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
