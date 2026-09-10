use ag_agent::{AgentModel, ReasoningLevel, SpeedMode};
use ag_session::SettingName;

use crate::connection::Database;

#[tokio::test]
async fn test_setting_round_trip_supports_default_smart_fast_and_review_models() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");

    database
        .settings()
        .upsert_setting(
            SettingName::DefaultSmartModel,
            AgentModel::Gemini31Pro.as_str(),
        )
        .await
        .expect("failed to persist default smart model");
    database
        .settings()
        .upsert_setting(SettingName::DefaultFastModel, AgentModel::Gpt56Sol.as_str())
        .await
        .expect("failed to persist default fast model");
    database
        .settings()
        .upsert_setting(
            SettingName::DefaultReviewModel,
            AgentModel::ClaudeOpus5.as_str(),
        )
        .await
        .expect("failed to persist default review model");

    // Act
    let default_smart_model = database
        .settings()
        .get_setting(SettingName::DefaultSmartModel)
        .await
        .expect("failed to load default smart model");
    let default_fast_model = database
        .settings()
        .get_setting(SettingName::DefaultFastModel)
        .await
        .expect("failed to load default fast model");
    let default_review_model = database
        .settings()
        .get_setting(SettingName::DefaultReviewModel)
        .await
        .expect("failed to load default review model");

    // Assert
    assert_eq!(
        default_smart_model,
        Some(AgentModel::Gemini31Pro.as_str().to_string())
    );
    assert_eq!(
        default_fast_model,
        Some(AgentModel::Gpt56Sol.as_str().to_string())
    );
    assert_eq!(
        default_review_model,
        Some(AgentModel::ClaudeOpus5.as_str().to_string())
    );
}

#[tokio::test]
async fn test_project_setting_round_trip_is_isolated_per_project() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let first_project_id = database
        .projects()
        .upsert_project("/tmp/project-a", Some("main".to_string()))
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project("/tmp/project-b", Some("main".to_string()))
        .await
        .expect("failed to insert second project");

    database
        .settings()
        .upsert_project_setting(
            first_project_id,
            SettingName::LaunchConfiguration,
            "npm run dev",
        )
        .await
        .expect("failed to persist first project setting");
    database
        .settings()
        .upsert_project_setting(
            second_project_id,
            SettingName::LaunchConfiguration,
            "cargo test",
        )
        .await
        .expect("failed to persist second project setting");

    // Act
    let first_project_setting = database
        .settings()
        .get_project_setting(first_project_id, SettingName::LaunchConfiguration)
        .await
        .expect("failed to load first project setting");
    let second_project_setting = database
        .settings()
        .get_project_setting(second_project_id, SettingName::LaunchConfiguration)
        .await
        .expect("failed to load second project setting");

    // Assert
    assert_eq!(first_project_setting, Some("npm run dev".to_string()));
    assert_eq!(second_project_setting, Some("cargo test".to_string()));
}

#[tokio::test]
async fn test_project_role_reasoning_levels_round_trip_with_typed_setting_helpers() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    // Act
    let role_reasoning_levels = [
        (
            SettingName::DefaultSmartReasoningLevel,
            ReasoningLevel::High,
        ),
        (SettingName::DefaultFastReasoningLevel, ReasoningLevel::Low),
        (
            SettingName::DefaultReviewReasoningLevel,
            ReasoningLevel::XHigh,
        ),
    ];
    for (name, reasoning_level) in role_reasoning_levels {
        database
            .settings()
            .upsert_project_setting(project_id, name, reasoning_level.as_str())
            .await
            .expect("failed to persist project role reasoning level");
    }
    let mut loaded_reasoning_levels = Vec::new();
    for (name, _) in role_reasoning_levels {
        loaded_reasoning_levels.push(
            database
                .settings()
                .load_project_reasoning_level(project_id, name)
                .await
                .expect("failed to load project role reasoning level"),
        );
    }

    // Assert
    assert_eq!(
        loaded_reasoning_levels,
        vec![
            ReasoningLevel::High,
            ReasoningLevel::Low,
            ReasoningLevel::XHigh,
        ]
    );
}

#[tokio::test]
async fn test_load_project_reasoning_level_defaults_when_setting_is_missing_or_invalid() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    // Act
    let missing_setting_level = database
        .settings()
        .load_project_reasoning_level(project_id, SettingName::DefaultSmartReasoningLevel)
        .await
        .expect("failed to load default project reasoning level");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartReasoningLevel,
            "unsupported",
        )
        .await
        .expect("failed to insert unsupported project reasoning level");
    let invalid_setting_level = database
        .settings()
        .load_project_reasoning_level(project_id, SettingName::DefaultSmartReasoningLevel)
        .await
        .expect("failed to load fallback project reasoning level");

    // Assert
    assert_eq!(missing_setting_level, ReasoningLevel::High);
    assert_eq!(invalid_setting_level, ReasoningLevel::High);
}

#[tokio::test]
async fn test_load_project_speed_mode_round_trips_and_defaults_invalid_values() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    // Act
    let missing_speed_mode = database
        .settings()
        .load_project_speed_mode(project_id, SettingName::DefaultSmartSpeedMode)
        .await
        .expect("failed to load missing speed mode");
    database
        .settings()
        .upsert_project_setting(project_id, SettingName::DefaultSmartSpeedMode, "fast")
        .await
        .expect("failed to persist speed mode");
    let fast_speed_mode = database
        .settings()
        .load_project_speed_mode(project_id, SettingName::DefaultSmartSpeedMode)
        .await
        .expect("failed to load speed mode");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartSpeedMode,
            "unsupported",
        )
        .await
        .expect("failed to persist invalid speed mode");
    let invalid_speed_mode = database
        .settings()
        .load_project_speed_mode(project_id, SettingName::DefaultSmartSpeedMode)
        .await
        .expect("failed to load invalid speed mode");

    // Assert
    assert_eq!(missing_speed_mode, SpeedMode::Normal);
    assert_eq!(fast_speed_mode, SpeedMode::Fast);
    assert_eq!(invalid_speed_mode, SpeedMode::Normal);
}

#[tokio::test]
async fn test_set_and_load_active_project_id_round_trip() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");

    // Act
    database
        .settings()
        .set_active_project_id(project_id)
        .await
        .expect("failed to persist active project id");
    let active_project_id = database
        .settings()
        .load_active_project_id()
        .await
        .expect("failed to load active project id");

    // Assert
    assert_eq!(active_project_id, Some(project_id));
}
