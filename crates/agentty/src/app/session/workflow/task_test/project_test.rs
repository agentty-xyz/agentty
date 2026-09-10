use ag_agent::AgentSelectionMetadata;

use super::super::SessionTaskService;
use super::support::insert_review_session;
use crate::db::AppRepositories;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::setting::SettingName;

#[tokio::test]
/// Verifies auto-commit prefers the project fast agent/model selection
/// before other fallback settings.
async fn test_load_auto_commit_agent_setting_prefers_project_fast_selection() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let project_id = database
        .sessions()
        .load_session_project_id("session-id")
        .await
        .expect("failed to load session project id")
        .expect("session should have project id");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultFastModel,
            AgentModel::Gemini31Pro.as_str(),
        )
        .await
        .expect("failed to persist default fast model");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultFastAgent,
            AgentKind::Antigravity.name(),
        )
        .await
        .expect("failed to persist default fast agent");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartModel,
            AgentModel::Gemini31Pro.as_str(),
        )
        .await
        .expect("failed to persist default smart model");

    // Act
    let auto_commit_agent = SessionTaskService::load_auto_commit_agent_setting(
        &database,
        "session-id",
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await;

    // Assert
    assert_eq!(
        auto_commit_agent,
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini31Pro)
    );
}

#[tokio::test]
/// Verifies auto-commit loads the reasoning effort paired with the project
/// fast model and defaults when the session has no project.
async fn test_load_auto_commit_reasoning_level_uses_project_fast_setting() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let project_id = database
        .sessions()
        .load_session_project_id("session-id")
        .await
        .expect("failed to load session project id")
        .expect("session should have project id");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultFastReasoningLevel,
            ReasoningLevel::Low.as_str(),
        )
        .await
        .expect("failed to persist default fast reasoning level");

    // Act
    let persisted_reasoning_level =
        SessionTaskService::load_auto_commit_reasoning_level(&database, "session-id").await;
    let missing_reasoning_level =
        SessionTaskService::load_auto_commit_reasoning_level(&database, "missing-session").await;

    // Assert
    assert_eq!(persisted_reasoning_level, ReasoningLevel::Low);
    assert_eq!(missing_reasoning_level, ReasoningLevel::High);
}

#[tokio::test]
/// Verifies auto-commit loads the speed paired with the project fast model
/// and defaults when the session has no project.
async fn test_load_auto_commit_speed_mode_uses_project_fast_setting() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let project_id = database
        .sessions()
        .load_session_project_id("session-id")
        .await
        .expect("failed to load session project id")
        .expect("session should have project id");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultFastSpeedMode,
            SpeedMode::Fast.as_str(),
        )
        .await
        .expect("failed to persist default fast speed mode");

    // Act
    let persisted_speed_mode =
        SessionTaskService::load_auto_commit_speed_mode(&database, "session-id").await;
    let missing_speed_mode =
        SessionTaskService::load_auto_commit_speed_mode(&database, "missing-session").await;

    // Assert
    assert_eq!(persisted_speed_mode, SpeedMode::Fast);
    assert_eq!(missing_speed_mode, SpeedMode::Normal);
}
