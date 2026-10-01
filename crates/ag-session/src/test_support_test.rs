//! Named model fixtures shared by session, transport, persistence, and host
//! tests.
//!
//! These selections are pinned independently of provider defaults. Update them
//! only when their model is retired; tests of actual defaults and model ids
//! should keep explicit expectations instead.

use crate::{AgentKind, AgentModel, AgentSelection};

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
