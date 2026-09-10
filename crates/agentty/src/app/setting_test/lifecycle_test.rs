use super::support::{select_row, settings_manager_with_launch_configuration, test_services};
use crate::domain::setting::SettingName;

#[tokio::test]
async fn delete_selected_launch_configuration_persists_remaining_commands() {
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
    manager.next_launch_configuration_list_editor_item();

    // Act
    manager.delete_selected_launch_configuration().await;

    // Assert
    assert_eq!(
        manager.settings().launch_configuration,
        "cargo test\nlazygit"
    );
    assert_eq!(
        services
            .db()
            .settings()
            .get_project_setting(project_id, SettingName::LaunchConfiguration)
            .await
            .expect("failed to load launch configuration"),
        Some("cargo test\nlazygit".to_string())
    );
}
