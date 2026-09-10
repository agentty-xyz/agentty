use super::super::super::agent::ReasoningLevel;
use super::super::Session;
use crate::test_support::SessionFixtureBuilder;

/// Builds a minimal session fixture for reasoning-level tests.
pub(super) fn test_session(reasoning_level_override: Option<ReasoningLevel>) -> Session {
    SessionFixtureBuilder::new()
        .reasoning_level_override(reasoning_level_override)
        .build()
}
