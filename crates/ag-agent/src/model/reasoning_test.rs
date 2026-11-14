use ag_contracts::ReasoningLevel;
use ag_session::AgentModel;

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
/// Ensures Antigravity reasoning values stay within each model's accepted
/// effort levels, as `agy` 1.5.8 validates them.
fn test_reasoning_level_antigravity_maps_all_levels_per_model() {
    // Arrange
    let cases = [
        (
            AgentModel::Gemini4Argon,
            [
                Some("low"),
                Some("medium"),
                Some("high"),
                Some("high"),
                Some("high"),
            ],
        ),
        (
            AgentModel::ClaudeOpus55,
            [
                Some("low"),
                Some("medium"),
                Some("high"),
                Some("high"),
                Some("high"),
            ],
        ),
        (
            AgentModel::Gemini4Hydrogen,
            [
                Some("low"),
                Some("medium"),
                Some("high"),
                Some("xhigh"),
                Some("xhigh"),
            ],
        ),
        (AgentModel::GptOss120b, [Some("medium"); 5]),
        (AgentModel::GeminiProOrchestrator, [None; 5]),
    ];

    for (model, expected_efforts) in cases {
        // Act
        let efforts =
            ReasoningLevel::ALL.map(|level| reasoning::antigravity(model.as_str(), level));

        // Assert
        assert_eq!(efforts, expected_efforts, "{}", model.as_str());
    }
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
