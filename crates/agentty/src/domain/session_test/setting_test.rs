use super::super::super::agent::ReasoningLevel;
use super::support::test_session;

#[test]
/// Ensures invalid rows without a stored value use the stable application
/// fallback rather than the current project setting.
fn test_effective_reasoning_level_uses_stable_fallback_when_value_is_missing() {
    // Arrange
    let session = test_session(None);

    // Act
    let effective_reasoning_level = session.effective_reasoning_level();
    // Assert
    assert_eq!(effective_reasoning_level, ReasoningLevel::High);
}

#[test]
/// Ensures sessions with an override use that override instead of the
/// provided default.
fn test_effective_reasoning_level_prefers_session_override() {
    // Arrange
    let session = test_session(Some(ReasoningLevel::High));

    // Act
    let effective_reasoning_level = session.effective_reasoning_level();
    // Assert
    assert_eq!(effective_reasoning_level, ReasoningLevel::High);
}

#[test]
/// Ensures clearing a session value uses the stable application fallback.
fn test_effective_reasoning_level_uses_stable_fallback_after_value_is_cleared() {
    // Arrange
    let mut session = test_session(Some(ReasoningLevel::XHigh));
    session.reasoning_level_override = None;

    // Act
    let effective_reasoning_level = session.effective_reasoning_level();
    // Assert
    assert_eq!(effective_reasoning_level, ReasoningLevel::High);
}
