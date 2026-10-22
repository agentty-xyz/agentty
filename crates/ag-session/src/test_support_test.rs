//! Named model fixtures shared by session, transport, persistence, and host
//! tests.
//!
//! These selections are pinned independently of provider defaults. Update them
//! only when their model is retired; tests of actual defaults and model ids
//! should keep explicit expectations instead.

use crate::{AgentDefinition, AgentKind, AgentModel, AgentSelection, PlanThenImplementAgents};

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

/// Supplies stable agent snapshots for workflow persistence and routing tests.
#[must_use]
pub fn workflow_agents() -> Box<PlanThenImplementAgents> {
    Box::new(PlanThenImplementAgents {
        implementer: agent_definition("implementer"),
        planner: agent_definition("planner"),
    })
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
