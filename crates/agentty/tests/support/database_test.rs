use ag_session::SettingName;
use agentty::app;
use agentty::db::Database;
use agentty::domain::agent::ReasoningLevel;

use super::{
    persist_active_project_id_for_test, persist_active_tab_for_test,
    persist_project_reasoning_levels_for_test,
};

#[tokio::test]
async fn persist_settings_for_test_upserts_values() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory database");

    // Act
    persist_active_project_id_for_test(&database, 41)
        .await
        .expect("failed to persist initial active project");
    persist_active_project_id_for_test(&database, 42)
        .await
        .expect("failed to update active project");
    persist_active_tab_for_test(&database, app::Tab::Projects)
        .await
        .expect("failed to persist initial active tab");
    persist_active_tab_for_test(&database, app::Tab::Sessions)
        .await
        .expect("failed to update active tab");
    let project_id = database
        .projects()
        .upsert_project("/tmp/reasoning-defaults", Some("main".to_string()))
        .await
        .expect("failed to create project");
    persist_project_reasoning_levels_for_test(
        &database,
        project_id,
        ReasoningLevel::Medium,
        ReasoningLevel::Low,
        ReasoningLevel::XHigh,
    )
    .await
    .expect("failed to persist role reasoning levels");

    // Assert
    assert_eq!(
        database
            .settings()
            .load_active_project_id()
            .await
            .expect("failed to load active project"),
        Some(42)
    );
    assert_eq!(
        database
            .settings()
            .get_setting(SettingName::ActiveTab)
            .await
            .expect("failed to load active tab")
            .as_deref(),
        Some("Sessions")
    );
    for (setting_name, expected_level) in [
        (
            SettingName::DefaultSmartReasoningLevel,
            ReasoningLevel::Medium,
        ),
        (SettingName::DefaultFastReasoningLevel, ReasoningLevel::Low),
        (
            SettingName::DefaultReviewReasoningLevel,
            ReasoningLevel::XHigh,
        ),
    ] {
        assert_eq!(
            database
                .settings()
                .load_project_reasoning_level(project_id, setting_name)
                .await
                .expect("failed to load role reasoning level"),
            expected_level
        );
    }
}
