use crate::session::SqliteSessionRepository;

#[test]
/// Ensures model-only inserts keep their provider routing, including harness
/// model ids that share no family prefix.
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
        ("muse-spark-1.3", "harness"),
        ("kimi-k3", "harness"),
        ("qwen-plus", "harness"),
        ("kimi-unlisted", "antigravity"),
        ("unrecognized-model", "antigravity"),
    ];

    // Act
    let agents = cases.map(|(model, _)| SqliteSessionRepository::persisted_agent_for_model(model));

    // Assert
    assert_eq!(agents, cases.map(|(_, expected)| expected.to_string()));
}
