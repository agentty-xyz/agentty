use super::support::{
    new_settings_manager, select_row, settings_manager, settings_manager_with_launch_configuration,
    test_services,
};
use crate::domain::input::InputCommand;
use crate::domain::setting::SettingName;
use crate::presentation::setting::LaunchConfigurationListEditorMode;

#[tokio::test]
async fn confirm_launch_configuration_input_adds_trimmed_command_and_persists_value() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager(&services, project_id).await;
    select_row(&mut manager, 7);
    manager.handle_enter();
    manager.start_adding_launch_configuration();

    // Act
    manager
        .apply_launch_configuration_input_command(InputCommand::InsertText(" nvim ".to_string()));
    manager.confirm_launch_configuration_input().await;

    // Assert
    assert_eq!(manager.settings().launch_configuration, "nvim");
    assert_eq!(
        services
            .db()
            .settings()
            .get_project_setting(project_id, SettingName::LaunchConfiguration)
            .await
            .expect("failed to load launch configuration"),
        Some("nvim".to_string())
    );
}

#[tokio::test]
async fn confirm_launch_configuration_input_edits_selected_command_and_persists_value() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager_with_launch_configuration(
        &services,
        project_id,
        "cargo test\nnpm run dev",
    )
    .await;
    select_row(&mut manager, 7);
    manager.handle_enter();
    manager.next_launch_configuration_list_editor_item();
    manager.start_editing_selected_launch_configuration();

    for _ in 0.."npm run dev".chars().count() {
        manager.apply_launch_configuration_input_command(InputCommand::DeleteBackward);
    }
    for character in "lazygit".chars() {
        manager.apply_launch_configuration_input_command(InputCommand::Insert(character));
    }

    // Act
    manager.confirm_launch_configuration_input().await;

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

#[tokio::test]
async fn confirm_launch_configuration_input_drops_empty_edited_command() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager_with_launch_configuration(
        &services,
        project_id,
        "cargo test\nnpm run dev",
    )
    .await;
    select_row(&mut manager, 7);
    manager.handle_enter();
    manager.start_editing_selected_launch_configuration();

    for _ in 0.."cargo test".chars().count() {
        manager.apply_launch_configuration_input_command(InputCommand::DeleteBackward);
    }

    // Act
    manager.confirm_launch_configuration_input().await;

    // Assert
    assert_eq!(manager.settings().launch_configuration, "npm run dev");
    assert_eq!(
        services
            .db()
            .settings()
            .get_project_setting(project_id, SettingName::LaunchConfiguration)
            .await
            .expect("failed to load launch configuration"),
        Some("npm run dev".to_string())
    );
}

#[test]
fn footer_hint_returns_launch_configuration_input_hint_when_input_is_active() {
    // Arrange
    let mut manager = new_settings_manager();
    select_row(&mut manager, 7);
    manager.handle_enter();
    manager.start_adding_launch_configuration();

    // Act
    let footer_hint = manager.footer_hint();

    // Assert
    assert_eq!(
        footer_hint,
        "Launch Configurations: type a command, Enter save, Esc cancel"
    );
}

#[test]
fn cancel_launch_configuration_input_returns_to_browse_without_changing_value() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().launch_configuration = "old command".to_string();
    select_row(&mut manager, 7);
    manager.handle_enter();
    manager.start_adding_launch_configuration();
    manager.apply_launch_configuration_input_command(InputCommand::Insert('n'));

    // Act
    manager.cancel_launch_configuration_input();

    // Assert
    let editor = manager
        .launch_configuration_list_editor()
        .expect("expected launch-configuration list editor");
    assert_eq!(manager.view.launch_configuration, "old command");
    assert_eq!(editor.mode, LaunchConfigurationListEditorMode::Browse);
    assert!(editor.input.is_none());
}
