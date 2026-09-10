use ag_agent::AgentSelectionMetadata;

use super::super::load_default_review_agent_setting;
use super::support::{new_settings_manager, test_services};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::setting::SettingName;

#[test]
fn settings_rows_show_default_review_model_value() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().default_review_selection =
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5);

    // Act
    let rows = manager.settings_rows();

    // Assert
    assert_eq!(rows[5].1, "claude/claude-opus-5 [xhigh, Normal]");
}

#[test]
fn setting_name_as_str_returns_default_review_model() {
    // Arrange

    // Act
    let setting_name = SettingName::DefaultReviewModel.as_str();

    // Assert
    assert_eq!(setting_name, "DefaultReviewModel");
}

#[test]
fn setting_name_as_str_returns_default_review_agent() {
    // Arrange

    // Act
    let setting_name = SettingName::DefaultReviewAgent.as_str();

    // Assert
    assert_eq!(setting_name, "DefaultReviewAgent");
}

#[tokio::test]
async fn load_default_review_agent_setting_uses_inactive_project_baseline() {
    // Arrange
    let (services, active_project_id) = test_services().await;
    let inactive_project_id = services
        .db()
        .projects()
        .upsert_project("/tmp/inactive-project", Some("main".to_string()))
        .await
        .expect("failed to create inactive project");
    services
        .db()
        .settings()
        .upsert_project_settings(
            active_project_id,
            vec![
                (
                    SettingName::DefaultSmartAgent,
                    AgentKind::Codex.name().to_string(),
                ),
                (
                    SettingName::DefaultSmartModel,
                    AgentModel::Gpt56Sol.as_str().to_string(),
                ),
            ],
        )
        .await
        .expect("failed to persist active project smart default");
    services
        .db()
        .settings()
        .set_active_project_id(active_project_id)
        .await
        .expect("failed to set active project");

    // Act
    let (selection, reasoning_level, speed_mode) =
        load_default_review_agent_setting(&services, inactive_project_id).await;

    // Assert
    assert_eq!(
        selection,
        AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31Pro)
    );
    assert_eq!(reasoning_level, ReasoningLevel::default());
    assert_eq!(speed_mode, SpeedMode::default());
}
