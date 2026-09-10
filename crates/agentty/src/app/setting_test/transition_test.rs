use super::support::{new_settings_manager, select_row, settings_manager, test_services};
use crate::domain::input::InputCommand;
use crate::domain::setting::SettingName;
use crate::presentation::setting::LaunchConfigurationListEditorMode;

#[test]
fn is_launch_configuration_list_editor_open_returns_false_by_default() {
    // Arrange
    let manager = new_settings_manager();

    // Act
    let is_open = manager.is_launch_configuration_list_editor_open();

    // Assert
    assert!(!is_open);
}

#[test]
fn footer_hint_returns_selector_dropdown_hint_when_dropdown_is_open() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.handle_enter();

    // Act
    let footer_hint = manager.footer_hint();

    // Assert
    assert_eq!(
        footer_hint,
        "Selecting setting value: j/k move, Enter select, Esc/q close"
    );
}

#[test]
fn next_and_previous_do_not_move_selection_while_launch_configuration_editor_is_open() {
    // Arrange
    let mut manager = new_settings_manager();
    select_row(&mut manager, 7);
    manager.handle_enter();

    // Act
    manager.next();
    manager.previous();

    // Assert
    assert_eq!(
        manager
            .presentation
            .snapshot(&manager.view)
            .selected_row_index,
        Some(7)
    );
    assert!(manager.is_launch_configuration_list_editor_open());
}

#[test]
fn next_and_previous_do_not_move_selection_while_selector_dropdown_is_open() {
    // Arrange
    let mut manager = new_settings_manager();
    select_row(&mut manager, 0);
    manager.handle_enter();

    // Act
    manager.next();
    manager.previous();

    // Assert
    assert_eq!(
        manager
            .presentation
            .snapshot(&manager.view)
            .selected_row_index,
        Some(0)
    );
    assert!(manager.is_selector_dropdown_open());
}

#[test]
fn previous_wraps_to_default_response_style_row_from_theme_row() {
    // Arrange
    let mut manager = new_settings_manager();

    // Act
    manager.previous();

    // Assert
    assert_eq!(
        manager
            .presentation
            .snapshot(&manager.view)
            .selected_row_index,
        Some(8)
    );
}

#[test]
fn next_moves_selection_to_orchestration_parallelism_row() {
    // Arrange
    let mut manager = new_settings_manager();

    // Act
    manager.next();

    // Assert
    assert_eq!(
        manager
            .presentation
            .snapshot(&manager.view)
            .selected_row_index,
        Some(1)
    );
}

#[test]
fn handle_enter_opens_launch_configuration_list_editor() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().launch_configuration = "nvim .".to_string();
    select_row(&mut manager, 7);

    // Act
    manager.handle_enter();

    // Assert
    let editor = manager
        .launch_configuration_list_editor()
        .expect("expected launch-configuration list editor");
    assert_eq!(editor.commands, vec!["nvim .".to_string()]);
    assert_eq!(editor.mode, LaunchConfigurationListEditorMode::Browse);
}

#[tokio::test]
async fn launch_configuration_editor_apis_are_noops_without_open_editor() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager(&services, project_id).await;

    // Act
    manager.start_adding_launch_configuration();
    manager.start_editing_selected_launch_configuration();
    manager.apply_launch_configuration_input_command(InputCommand::Insert('n'));
    manager.apply_launch_configuration_input_command(InputCommand::DeleteBackward);
    manager.apply_launch_configuration_input_command(InputCommand::DeleteForward);
    manager.apply_launch_configuration_input_command(InputCommand::MoveLeft);
    manager.apply_launch_configuration_input_command(InputCommand::MoveRight);
    manager.apply_launch_configuration_input_command(InputCommand::MoveHome);
    manager.apply_launch_configuration_input_command(InputCommand::MoveEnd);
    manager.confirm_launch_configuration_input().await;
    manager.delete_selected_launch_configuration().await;
    manager.move_selected_launch_configuration_down().await;
    manager.move_selected_launch_configuration_up().await;

    // Assert
    assert_eq!(manager.settings().launch_configuration, "");
    assert!(!manager.is_selector_dropdown_open());
    assert_eq!(
        services
            .db()
            .settings()
            .get_project_setting(project_id, SettingName::LaunchConfiguration)
            .await
            .expect("failed to load launch configuration"),
        None
    );
}

#[test]
fn launch_configurations_returns_single_trimmed_command() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().launch_configuration = "  cargo test  ".to_string();

    // Act
    let launch_configurations = manager.launch_configurations();

    // Assert
    assert_eq!(launch_configurations, vec!["cargo test".to_string()]);
}

#[test]
fn launch_configurations_splits_newline_entries() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().launch_configuration = " cargo test \n npm run dev \n".to_string();

    // Act
    let launch_configurations = manager.launch_configurations();

    // Assert
    assert_eq!(
        launch_configurations,
        vec!["cargo test".to_string(), "npm run dev".to_string()]
    );
}

#[test]
fn launch_configurations_does_not_split_double_pipe_entries() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().launch_configuration = "cargo test || npm run dev".to_string();

    // Act
    let launch_configurations = manager.launch_configurations();

    // Assert
    assert_eq!(
        launch_configurations,
        vec!["cargo test || npm run dev".to_string()]
    );
}
