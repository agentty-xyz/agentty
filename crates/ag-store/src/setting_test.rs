use ag_agent::{AgentModel, ResponseStyle};
use ag_session::SettingName;

use crate::AppRepositories;

#[tokio::test]
async fn project_response_style_loads_persisted_value_and_defaults_invalid_values() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("db should open");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    let missing_style = repositories
        .settings()
        .load_project_response_style(project_id, SettingName::DefaultResponseStyle)
        .await
        .expect("missing style should load");
    repositories
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultResponseStyle,
            ResponseStyle::Detailed.as_str(),
        )
        .await
        .expect("detailed style should persist");

    // Act
    let detailed_style = repositories
        .settings()
        .load_project_response_style(project_id, SettingName::DefaultResponseStyle)
        .await
        .expect("detailed style should load");
    repositories
        .settings()
        .upsert_project_setting(project_id, SettingName::DefaultResponseStyle, "invalid")
        .await
        .expect("invalid fixture should persist");
    let invalid_style = repositories
        .settings()
        .load_project_response_style(project_id, SettingName::DefaultResponseStyle)
        .await
        .expect("invalid style should default");

    // Assert
    assert_eq!(missing_style, ResponseStyle::Balanced);
    assert_eq!(detailed_style, ResponseStyle::Detailed);
    assert_eq!(invalid_style, ResponseStyle::Balanced);
}

#[tokio::test]
/// Verifies project setting batches roll back when any setting write fails.
async fn test_upsert_project_settings_rolls_back_partial_batch() {
    // Arrange
    let (repositories, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    sqlx::query!(
        r"
CREATE TRIGGER fail_default_fast_agent_insert
BEFORE INSERT ON project_setting
WHEN NEW.name = 'DefaultFastAgent'
BEGIN
    SELECT RAISE(FAIL, 'forced setting failure');
END;
"
    )
    .execute(&pool)
    .await
    .expect("failed to install failure trigger");

    // Act
    let result = repositories
        .settings()
        .upsert_project_settings(
            project_id,
            vec![
                (
                    SettingName::DefaultFastModel,
                    AgentModel::Gemini31Pro.as_str().to_string(),
                ),
                (SettingName::DefaultFastAgent, "antigravity".to_string()),
            ],
        )
        .await;

    // Assert
    assert!(result.is_err());
    assert_eq!(
        repositories
            .settings()
            .get_project_setting(project_id, SettingName::DefaultFastModel)
            .await
            .expect("failed to load fast model setting"),
        None
    );
    assert_eq!(
        repositories
            .settings()
            .get_project_setting(project_id, SettingName::DefaultFastAgent)
            .await
            .expect("failed to load fast agent setting"),
        None
    );
}
