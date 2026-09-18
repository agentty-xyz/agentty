use ag_contracts::ReasoningLevel;

use crate::model::reasoning;

#[test]
/// Ensures `reasoning::claude()` maps all levels to the correct
/// Claude `--effort` values, including the highest generic levels.
fn test_reasoning_level_claude_maps_all_levels() {
    // Arrange / Act / Assert
    assert_eq!(reasoning::claude(ReasoningLevel::Low), "low");
    assert_eq!(reasoning::claude(ReasoningLevel::Medium), "medium");
    assert_eq!(reasoning::claude(ReasoningLevel::High), "high");
    assert_eq!(reasoning::claude(ReasoningLevel::XHigh), "max");
    assert_eq!(reasoning::claude(ReasoningLevel::Max), "max");
}

#[test]
/// Ensures Antigravity reasoning values stay within the CLI's accepted
/// `low`, `medium`, and `high` effort levels.
fn test_reasoning_level_antigravity_maps_all_levels() {
    // Arrange / Act / Assert
    assert_eq!(reasoning::antigravity(ReasoningLevel::Low), "low");
    assert_eq!(reasoning::antigravity(ReasoningLevel::Medium), "medium");
    assert_eq!(reasoning::antigravity(ReasoningLevel::High), "high");
    assert_eq!(reasoning::antigravity(ReasoningLevel::XHigh), "high");
    assert_eq!(reasoning::antigravity(ReasoningLevel::Max), "high");
}

#[test]
/// Ensures Codex reasoning values include the distinct `max` effort.
fn test_reasoning_level_codex_maps_all_levels() {
    // Arrange / Act / Assert
    assert_eq!(reasoning::codex(ReasoningLevel::Low), "low");
    assert_eq!(reasoning::codex(ReasoningLevel::Medium), "medium");
    assert_eq!(reasoning::codex(ReasoningLevel::High), "high");
    assert_eq!(reasoning::codex(ReasoningLevel::XHigh), "xhigh");
    assert_eq!(reasoning::codex(ReasoningLevel::Max), "max");
}
