use ag_session::{AgentKind, AgentModel, AgentSelection, parse_persisted_session_agent_model};

use crate::AppRepositories;
use crate::session::SqliteSessionRepository;

#[test]
/// Ensures model-only inserts keep their provider routing, including harness
/// model ids that share no family prefix and catalog ids whose prefix names
/// another provider family.
fn model_inference_preserves_current_retired_and_unknown_family_routing() {
    // Arrange
    let cases = [
        ("claude-opus-5", "claude"),
        ("claude-opus-4-6", "claude"),
        ("claude-unlisted", "claude"),
        ("gpt-5.6-sol", "codex"),
        ("gpt-5.4", "codex"),
        ("gpt-unlisted", "codex"),
        ("gpt-oss-120b", "antigravity"),
        ("gemini-4-argon", "antigravity"),
        ("gemini-3-pro-preview", "antigravity"),
        ("gemini-unlisted", "antigravity"),
        ("muse-spark-1.3", "harness"),
        ("kimi-k3", "harness"),
        ("qwen-plus", "harness"),
        ("openai-compatible", "harness"),
        ("kimi-unlisted", "antigravity"),
        ("unrecognized-model", "antigravity"),
    ];

    // Act
    let agents = cases.map(|(model, _)| SqliteSessionRepository::persisted_agent_for_model(model));

    // Assert
    assert_eq!(agents, cases.map(|(_, expected)| expected.to_string()));
}

#[tokio::test]
/// Ensures a model-only insert of Antigravity's `gpt-oss-120b` loads back as
/// the same model instead of being routed to Codex by its `gpt-` prefix and
/// replaced with the Codex default.
async fn test_insert_session_round_trips_gpt_oss_model() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    let sessions = database.sessions();

    // Act
    sessions
        .insert_session(
            "session-a",
            AgentModel::GptOss120b.as_str(),
            "main",
            "Draft",
            project_id,
        )
        .await
        .expect("failed to insert session");
    let row = sessions
        .load_session("session-a")
        .await
        .expect("session should load")
        .expect("session should exist");
    let selection = parse_persisted_session_agent_model(Some(&row.agent), &row.model);

    // Assert
    assert_eq!(
        selection,
        AgentSelection::new(AgentKind::Antigravity, AgentModel::GptOss120b)
    );
}
