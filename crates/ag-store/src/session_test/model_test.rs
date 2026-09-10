use crate::session::SqliteSessionRepository;

#[test]
fn model_inference_preserves_current_retired_and_unknown_family_routing() {
    // Arrange
    let cases = [
        ("claude-opus-5", "claude"),
        ("claude-opus-4-6", "claude"),
        ("claude-unlisted", "claude"),
        ("gpt-5.6-sol", "codex"),
        ("gpt-5.4", "codex"),
        ("gpt-unlisted", "codex"),
        ("gemini-3.1-pro-preview", "antigravity"),
        ("gemini-3-pro-preview", "antigravity"),
        ("gemini-unlisted", "antigravity"),
        ("unrecognized-model", "antigravity"),
    ];

    // Act
    let agents = cases.map(|(model, _)| SqliteSessionRepository::persisted_agent_for_model(model));

    // Assert
    assert_eq!(agents, cases.map(|(_, expected)| expected.to_string()));
}
