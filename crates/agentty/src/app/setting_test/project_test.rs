use super::super::{load_default_smart_model_setting, load_project_speed_mode_setting};
use super::support::{new_settings_manager, settings_manager, test_services};
use crate::db::AppRepositories;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::setting::SettingName;
use crate::domain::theme::ColorTheme;

#[tokio::test]
async fn project_speed_default_without_project_uses_normal() {
    // Arrange
    let repositories = AppRepositories::in_memory()
        .await
        .expect("database should open");

    // Act
    let speed_mode =
        load_project_speed_mode_setting(&repositories, None, SettingName::DefaultSmartSpeedMode)
            .await;

    // Assert
    assert_eq!(speed_mode, SpeedMode::Normal);
}

#[test]
fn settings_rows_split_theme_into_global_and_project_sections() {
    // Arrange
    let manager = new_settings_manager();

    // Act
    let global_rows = manager.global_settings_rows();
    let project_rows = manager.project_settings_rows();

    // Assert
    assert_eq!(global_rows.len(), 3);
    assert_eq!(global_rows[0].0, "Theme");
    assert_eq!(global_rows[1].0, "Orchestrator Parallelism");
    assert_eq!(global_rows[2].0, "Auto-approve Research");
    assert_eq!(project_rows.len(), 6);
    assert_eq!(project_rows[0].0, "Default Smart Model");
    assert_eq!(project_rows[1].0, "Default Fast Model");
    assert_eq!(project_rows[2].0, "Default Review Model");
    assert_eq!(project_rows[3].0, "Coauthored by Agentty");
    assert_eq!(project_rows[4].0, "Launch Configurations");
    assert_eq!(project_rows[5].0, "Default Response Style");
}

#[tokio::test]
async fn settings_manager_new_loads_project_scoped_values() {
    // Arrange
    let (services, project_id) = test_services().await;
    services
        .db()
        .settings()
        .upsert_project_settings(
            project_id,
            vec![
                (
                    SettingName::DefaultSmartModel,
                    AgentModel::Gpt56Sol.as_str().to_string(),
                ),
                (
                    SettingName::DefaultFastModel,
                    AgentModel::Gpt53CodexSpark.as_str().to_string(),
                ),
                (
                    SettingName::DefaultReviewModel,
                    AgentModel::ClaudeOpus5.as_str().to_string(),
                ),
                (
                    SettingName::DefaultSmartReasoningLevel,
                    ReasoningLevel::High.as_str().to_string(),
                ),
                (
                    SettingName::DefaultFastReasoningLevel,
                    ReasoningLevel::Low.as_str().to_string(),
                ),
                (
                    SettingName::DefaultReviewReasoningLevel,
                    ReasoningLevel::XHigh.as_str().to_string(),
                ),
                (
                    SettingName::DefaultSmartSpeedMode,
                    SpeedMode::Fast.as_str().to_string(),
                ),
                (
                    SettingName::DefaultFastSpeedMode,
                    SpeedMode::Fast.as_str().to_string(),
                ),
                (
                    SettingName::DefaultReviewSpeedMode,
                    SpeedMode::Fast.as_str().to_string(),
                ),
                (SettingName::IncludeCoauthoredByAgentty, "false".to_string()),
                (SettingName::LaunchConfiguration, "nvim .".to_string()),
                (SettingName::LastUsedModelAsDefault, "true".to_string()),
            ],
        )
        .await
        .expect("failed to persist project settings");
    services
        .db()
        .settings()
        .upsert_setting(SettingName::Theme, ColorTheme::Green.as_str())
        .await
        .expect("failed to persist theme setting");
    services
        .db()
        .settings()
        .upsert_setting(SettingName::OrchestrationParallelism, "5")
        .await
        .expect("failed to persist orchestration parallelism");
    services
        .db()
        .settings()
        .upsert_setting(SettingName::AutoApproveOrchestrationResearch, "false")
        .await
        .expect("failed to persist research auto-approval");

    // Act
    let manager = settings_manager(&services, project_id).await;
    let settings = manager.settings();

    // Assert
    assert_eq!(
        settings.default_smart_selection,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol)
    );
    assert_eq!(
        settings.default_fast_selection,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol)
    );
    assert_eq!(
        settings.default_review_selection,
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5)
    );
    assert_eq!(settings.launch_configuration, "nvim .");
    assert_eq!(settings.default_smart_reasoning_level, ReasoningLevel::High);
    assert_eq!(settings.default_fast_reasoning_level, ReasoningLevel::Low);
    assert_eq!(
        settings.default_review_reasoning_level,
        ReasoningLevel::XHigh
    );
    assert_eq!(settings.default_smart_speed_mode, SpeedMode::Fast);
    assert_eq!(settings.default_fast_speed_mode, SpeedMode::Fast);
    assert_eq!(settings.default_review_speed_mode, SpeedMode::Fast);
    assert_eq!(settings.orchestration_parallelism, 5);
    assert!(!settings.auto_approve_orchestration_research);
    assert_eq!(settings.theme, ColorTheme::Green);
    assert!(!settings.include_coauthored_by_agentty);
    assert!(settings.use_last_used_model_as_default);
}

#[tokio::test]
async fn load_default_smart_model_setting_prefers_project_override() {
    // Arrange
    let (services, project_id) = test_services().await;
    services
        .db()
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartModel,
            AgentModel::Gpt56Sol.as_str(),
        )
        .await
        .expect("failed to persist smart model");

    // Act
    let loaded_model = load_default_smart_model_setting(
        &services,
        Some(project_id),
        AgentModel::ClaudeHaiku4520251001,
    )
    .await;

    // Assert
    assert_eq!(loaded_model, AgentModel::Gpt56Sol);
}
