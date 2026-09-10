use super::support::{
    new_settings_manager, select_row, settings_manager, settings_manager_with_launch_configuration,
    test_services,
};
use crate::domain::agent::ResponseStyle;
use crate::domain::setting::{MAX_ORCHESTRATION_PARALLELISM, SettingName};
use crate::presentation::setting::{SettingsAction, SettingsOperation};

#[tokio::test]
async fn move_selected_launch_configuration_down_persists_reordered_commands() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager_with_launch_configuration(
        &services,
        project_id,
        "cargo test\nnpm run dev\nlazygit",
    )
    .await;
    select_row(&mut manager, 7);
    manager.handle_enter();

    // Act
    manager.move_selected_launch_configuration_down().await;

    // Assert
    let editor = manager
        .launch_configuration_list_editor()
        .expect("expected launch-configuration list editor");
    assert_eq!(
        manager.settings().launch_configuration,
        "npm run dev\ncargo test\nlazygit"
    );
    assert_eq!(editor.selected_index, 1);
    assert_eq!(
        services
            .db()
            .settings()
            .get_project_setting(project_id, SettingName::LaunchConfiguration)
            .await
            .expect("failed to load launch configuration"),
        Some("npm run dev\ncargo test\nlazygit".to_string())
    );
}

#[tokio::test]
async fn selector_dropdown_selects_default_response_style_and_persists_value() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager(&services, project_id).await;
    select_row(&mut manager, 8);

    // Act
    manager.handle_enter();
    manager.next_selector_dropdown_option();
    manager.select_selector_dropdown_option().await;

    // Assert
    assert_eq!(
        manager.settings().default_response_style,
        ResponseStyle::Detailed
    );
    assert!(!manager.is_selector_dropdown_open());
    assert_eq!(
        services
            .db()
            .settings()
            .get_project_setting(project_id, SettingName::DefaultResponseStyle)
            .await
            .expect("failed to load response style setting"),
        Some("detailed".to_string())
    );
}

#[tokio::test]
async fn selector_dropdown_persists_bounded_orchestration_parallelism() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager(&services, project_id).await;
    select_row(&mut manager, 1);

    // Act
    manager.handle_enter();
    manager.next_selector_dropdown_option();
    manager.select_selector_dropdown_option().await;
    manager
        .persist_operation(SettingsOperation::OrchestrationParallelism(u8::MAX))
        .await;

    // Assert
    assert_eq!(
        manager.settings().orchestration_parallelism,
        MAX_ORCHESTRATION_PARALLELISM
    );
    assert_eq!(
        services
            .db()
            .settings()
            .get_setting(SettingName::OrchestrationParallelism)
            .await
            .expect("failed to load orchestration parallelism"),
        Some(MAX_ORCHESTRATION_PARALLELISM.to_string())
    );
}

#[tokio::test]
async fn selector_dropdown_persists_research_auto_approval() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager(&services, project_id).await;
    select_row(&mut manager, 2);

    // Act
    manager.handle_enter();
    manager.next_selector_dropdown_option();
    manager.select_selector_dropdown_option().await;

    // Assert
    assert!(!manager.settings().auto_approve_orchestration_research);
    assert_eq!(
        services
            .db()
            .settings()
            .get_setting(SettingName::AutoApproveOrchestrationResearch)
            .await
            .expect("failed to load research auto-approval"),
        Some("false".to_string())
    );
}

#[test]
fn navigation_actions_do_not_request_launch_configuration_persistence() {
    // Arrange
    let mut manager = new_settings_manager();
    select_row(&mut manager, 7);
    manager.handle_enter();

    // Act
    let next_operation = manager.apply(SettingsAction::Next);
    let previous_operation = manager.apply(SettingsAction::Previous);

    // Assert
    assert_eq!(next_operation, None);
    assert_eq!(previous_operation, None);
}
