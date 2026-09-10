use super::{
    LaunchConfigurationListEditorMode, LaunchConfigurationListEditorState, SelectorDropdownStage,
    SelectorDropdownState, SettingRow, SettingSelectorOption, SettingSelectorValue, SettingsAction,
    SettingsInput, SettingsOperation, SettingsPresentationState, SettingsView,
    apply_launch_configuration_input, display_last_used_model_value, display_model_selector_value,
    move_launch_configuration_list_editor_selection, reasoning_selector_option_labels,
    selector_options_for_row, settings_operation_for_model_selector,
    settings_operation_for_primary_selector, speed_selector_option_labels,
};
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode,
};
use crate::domain::input::{InputCommand, InputState};
use crate::domain::setting::MAX_ORCHESTRATION_PARALLELISM;
use crate::domain::theme::ColorTheme;

fn test_settings_view(launch_configuration: &str) -> SettingsView {
    let smart_selection = AgentSelection::new(
        AgentKind::Antigravity,
        AgentKind::Antigravity.default_model(),
    );

    SettingsView {
        available_model_selections: vec![
            smart_selection,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5),
        ],
        auto_approve_orchestration_research: true,
        default_fast_reasoning_level: ReasoningLevel::Low,
        default_fast_selection: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        default_fast_speed_mode: SpeedMode::Fast,
        default_review_reasoning_level: ReasoningLevel::XHigh,
        default_review_selection: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5),
        default_review_speed_mode: SpeedMode::Normal,
        default_response_style: ResponseStyle::Balanced,
        default_smart_reasoning_level: ReasoningLevel::High,
        default_smart_selection: smart_selection,
        default_smart_speed_mode: SpeedMode::Normal,
        include_coauthored_by_agentty: false,
        launch_configuration: launch_configuration.to_string(),
        orchestration_parallelism: 3,
        theme: ColorTheme::Current,
        use_last_used_model_as_default: false,
    }
}

fn select_row(state: &mut SettingsPresentationState, view: &SettingsView, row_index: usize) {
    for _ in 0..row_index {
        let operation = state.apply(view, SettingsAction::Next);
        assert_eq!(operation, None);
    }
}

/// Opens the launch-configuration editor in browse mode.
fn launch_configuration_editor_state(view: &SettingsView) -> SettingsPresentationState {
    let mut state = SettingsPresentationState::default();
    select_row(
        &mut state,
        view,
        SettingRow::LaunchConfiguration.table_index(),
    );
    let _ = state.apply(view, SettingsAction::Activate);

    state
}

#[test]
fn action_for_input_maps_selector_inputs() {
    // Arrange
    let view = test_settings_view("");
    let mut state = SettingsPresentationState::default();
    let _ = state.apply(&view, SettingsAction::Activate);
    let mappings = [
        (SettingsInput::Cancel, Some(SettingsAction::Cancel)),
        (SettingsInput::Character('q'), Some(SettingsAction::Cancel)),
        (SettingsInput::Character('Q'), Some(SettingsAction::Cancel)),
        (SettingsInput::Character('j'), Some(SettingsAction::Next)),
        (SettingsInput::MoveDown, Some(SettingsAction::Next)),
        (
            SettingsInput::Character('k'),
            Some(SettingsAction::Previous),
        ),
        (SettingsInput::MoveUp, Some(SettingsAction::Previous)),
        (SettingsInput::Confirm, Some(SettingsAction::Confirm)),
        (SettingsInput::Character('x'), None),
        (SettingsInput::Edit(InputCommand::MoveLeft), None),
    ];

    // Act / Assert
    for (input, expected_action) in mappings {
        assert_eq!(state.action_for_input(input), expected_action);
    }
}

#[test]
fn action_for_input_maps_launch_configuration_browse_inputs() {
    // Arrange
    let view = test_settings_view("cargo test\ncargo check");
    let state = launch_configuration_editor_state(&view);
    let mappings = [
        (SettingsInput::Cancel, Some(SettingsAction::Cancel)),
        (SettingsInput::Character('q'), Some(SettingsAction::Cancel)),
        (SettingsInput::Character('Q'), Some(SettingsAction::Cancel)),
        (SettingsInput::Character('j'), Some(SettingsAction::Next)),
        (SettingsInput::MoveDown, Some(SettingsAction::Next)),
        (
            SettingsInput::Character('k'),
            Some(SettingsAction::Previous),
        ),
        (SettingsInput::MoveUp, Some(SettingsAction::Previous)),
        (
            SettingsInput::Character('J'),
            Some(SettingsAction::MoveLaunchConfigurationDown),
        ),
        (
            SettingsInput::Character('K'),
            Some(SettingsAction::MoveLaunchConfigurationUp),
        ),
        (
            SettingsInput::Character('a'),
            Some(SettingsAction::StartAddingLaunchConfiguration),
        ),
        (
            SettingsInput::Character('e'),
            Some(SettingsAction::EditLaunchConfiguration),
        ),
        (
            SettingsInput::Confirm,
            Some(SettingsAction::EditLaunchConfiguration),
        ),
        (
            SettingsInput::Character('d'),
            Some(SettingsAction::DeleteLaunchConfiguration),
        ),
        (SettingsInput::Character('x'), None),
        (SettingsInput::Edit(InputCommand::MoveLeft), None),
    ];

    // Act / Assert
    for (input, expected_action) in mappings {
        assert_eq!(state.action_for_input(input), expected_action);
    }
}

#[test]
fn action_for_input_maps_launch_configuration_text_input() {
    // Arrange
    let view = test_settings_view("");
    let mut state = launch_configuration_editor_state(&view);
    let _ = state.apply(&view, SettingsAction::StartAddingLaunchConfiguration);

    // Act
    let confirm = state.action_for_input(SettingsInput::Confirm);
    let cancel = state.action_for_input(SettingsInput::Cancel);
    let character = state.action_for_input(SettingsInput::Character('x'));
    let edit = state.action_for_input(SettingsInput::Edit(InputCommand::DeleteBackward));
    let move_down = state.action_for_input(SettingsInput::MoveDown);
    let move_up = state.action_for_input(SettingsInput::MoveUp);

    // Assert
    assert_eq!(confirm, Some(SettingsAction::Confirm));
    assert_eq!(cancel, Some(SettingsAction::Cancel));
    assert_eq!(
        character,
        Some(SettingsAction::Input(InputCommand::Insert('x')))
    );
    assert_eq!(
        edit,
        Some(SettingsAction::Input(InputCommand::DeleteBackward))
    );
    assert_eq!(
        move_down,
        Some(SettingsAction::Input(InputCommand::MoveDown))
    );
    assert_eq!(move_up, Some(SettingsAction::Input(InputCommand::MoveUp)));
}

#[test]
fn action_for_input_ignores_input_without_open_overlay() {
    // Arrange
    let state = SettingsPresentationState::default();

    // Act
    let action = state.action_for_input(SettingsInput::Confirm);

    // Assert
    assert_eq!(action, None);
}

#[test]
fn action_for_paste_normalizes_active_single_line_input() {
    // Arrange
    let view = test_settings_view("");
    let mut active_state = launch_configuration_editor_state(&view);
    let _ = active_state.apply(&view, SettingsAction::StartAddingLaunchConfiguration);
    let inactive_state = SettingsPresentationState::default();

    // Act
    let active_action = active_state.action_for_paste("cargo test\r\nignored");
    let inactive_action = inactive_state.action_for_paste("cargo test");

    // Assert
    assert_eq!(
        active_action,
        Some(SettingsAction::Input(InputCommand::InsertText(
            "cargo test".to_string()
        )))
    );
    assert_eq!(inactive_action, None);
}

#[test]
fn activate_reuses_open_selector_and_launch_editor() {
    // Arrange
    let empty_view = test_settings_view("");
    let mut selector_state = SettingsPresentationState::default();
    let mut editor_state = SettingsPresentationState::default();
    select_row(&mut editor_state, &empty_view, 7);

    // Act
    let opened_selector = selector_state.apply(&empty_view, SettingsAction::Activate);
    let selector_operation = selector_state.apply(&empty_view, SettingsAction::Activate);
    let opened_editor = editor_state.apply(&empty_view, SettingsAction::Activate);
    let editor_operation = editor_state.apply(&empty_view, SettingsAction::Activate);

    // Assert
    assert_eq!(opened_selector, None);
    assert_eq!(
        selector_operation,
        Some(SettingsOperation::Theme(ColorTheme::Current))
    );
    assert_eq!(opened_editor, None);
    assert_eq!(editor_operation, None);
    assert!(editor_state.is_launch_configuration_list_editor_input_active());
}

#[test]
fn confirm_edits_and_saves_browse_editor() {
    // Arrange
    let view = test_settings_view("cargo test");
    let mut state = SettingsPresentationState::default();
    select_row(&mut state, &view, 7);
    let _ = state.apply(&view, SettingsAction::Activate);

    // Act
    let edit_operation = state.apply(&view, SettingsAction::Confirm);
    let save_operation = state.apply(&view, SettingsAction::Confirm);

    // Assert
    assert_eq!(edit_operation, None);
    assert_eq!(
        save_operation,
        Some(SettingsOperation::LaunchConfiguration(
            "cargo test".to_string()
        ))
    );
}

#[test]
fn launch_editor_rejects_invalid_delete_and_reorder_actions() {
    // Arrange
    let one_command_view = test_settings_view("cargo test");
    let mut one_command_state = SettingsPresentationState::default();
    select_row(&mut one_command_state, &one_command_view, 7);
    let _ = one_command_state.apply(&one_command_view, SettingsAction::Activate);
    let two_command_view = test_settings_view("cargo test\nnpm run dev");
    let mut two_command_state = SettingsPresentationState::default();
    select_row(&mut two_command_state, &two_command_view, 7);
    let _ = two_command_state.apply(&two_command_view, SettingsAction::Activate);
    let _ = two_command_state.apply(&two_command_view, SettingsAction::Next);

    // Act
    let single_reorder = one_command_state.apply(
        &one_command_view,
        SettingsAction::MoveLaunchConfigurationDown,
    );
    let delete_operation =
        one_command_state.apply(&one_command_view, SettingsAction::DeleteLaunchConfiguration);
    let empty_delete =
        one_command_state.apply(&one_command_view, SettingsAction::DeleteLaunchConfiguration);
    let move_up =
        two_command_state.apply(&two_command_view, SettingsAction::MoveLaunchConfigurationUp);
    let first_row_move_up =
        two_command_state.apply(&two_command_view, SettingsAction::MoveLaunchConfigurationUp);

    // Assert
    assert_eq!(single_reorder, None);
    assert_eq!(
        delete_operation,
        Some(SettingsOperation::LaunchConfiguration(String::new()))
    );
    assert_eq!(empty_delete, None);
    assert_eq!(
        move_up,
        Some(SettingsOperation::LaunchConfiguration(
            "npm run dev\ncargo test".to_string()
        ))
    );
    assert_eq!(first_row_move_up, None);
}

#[test]
fn empty_selector_options_close_or_remain_closed() {
    // Arrange
    let view = test_settings_view("");
    let mut closed_state = SettingsPresentationState::default();
    let mut stale_state = SettingsPresentationState {
        selector_dropdown: Some(SelectorDropdownState {
            row: SettingRow::LaunchConfiguration,
            selected_index: 0,
            stage: SelectorDropdownStage::Primary,
        }),
        ..SettingsPresentationState::default()
    };

    // Act
    closed_state.open_selector_dropdown(&view, SettingRow::LaunchConfiguration);
    let navigation_operation = stale_state.apply(&view, SettingsAction::Next);

    // Assert
    assert!(!closed_state.is_selector_dropdown_open());
    assert_eq!(navigation_operation, None);
    assert!(!stale_state.is_selector_dropdown_open());
}

#[test]
fn selectors_cover_role_reasoning_speed_and_invalid_pairs() {
    // Arrange
    let view = test_settings_view("");

    // Act
    let operations = [
        (
            3,
            SettingsOperation::DefaultSmartSelection {
                reasoning_level: view.default_smart_reasoning_level,
                selection: view.default_smart_selection,
                speed_mode: view.default_smart_speed_mode,
                use_last_used_model_as_default: false,
            },
        ),
        (
            4,
            SettingsOperation::DefaultFastSelection {
                reasoning_level: view.default_fast_reasoning_level,
                selection: view.default_fast_selection,
                speed_mode: view.default_fast_speed_mode,
            },
        ),
        (
            5,
            SettingsOperation::DefaultReviewSelection {
                reasoning_level: view.default_review_reasoning_level,
                selection: view.default_review_selection,
                speed_mode: view.default_review_speed_mode,
            },
        ),
    ]
    .map(|(row_index, expected_operation)| {
        let mut state = SettingsPresentationState::default();
        select_row(&mut state, &view, row_index);
        let _ = state.apply(&view, SettingsAction::Activate);

        let model_operation = state.apply(&view, SettingsAction::Confirm);
        let reasoning_operation = state.apply(&view, SettingsAction::Confirm);
        let speed_operation = if reasoning_operation.is_none() {
            state.apply(&view, SettingsAction::Confirm)
        } else {
            None
        };

        (
            model_operation,
            reasoning_operation.or(speed_operation),
            expected_operation,
        )
    });
    let mismatched_option = SettingSelectorOption {
        label: "Enabled".to_string(),
        value: SettingSelectorValue::Bool(true),
    };
    let is_mismatched_current = mismatched_option.is_current_for(&view, SettingRow::Theme);
    let invalid_model_operation = settings_operation_for_model_selector(
        SettingRow::Theme,
        view.default_smart_selection,
        ReasoningLevel::High,
        SpeedMode::Normal,
        false,
    );
    let invalid_operation = settings_operation_for_primary_selector(
        SettingRow::Theme,
        SettingSelectorValue::Bool(true),
    );
    let nonmodel_reasoning_level = SettingRow::Theme.reasoning_level(&view);
    let nonmodel_speed_mode = SettingRow::Theme.speed_mode(&view);

    // Assert
    for (model_operation, reasoning_operation, expected_operation) in operations {
        assert_eq!(model_operation, None);
        assert_eq!(reasoning_operation, Some(expected_operation));
    }
    assert!(!is_mismatched_current);
    assert_eq!(invalid_model_operation, None);
    assert_eq!(invalid_operation, None);
    assert_eq!(nonmodel_reasoning_level, None);
    assert_eq!(nonmodel_speed_mode, None);
}

#[test]
fn launch_input_handles_empty_edit_and_empty_add() {
    // Arrange
    let mut empty_edit = LaunchConfigurationListEditorState {
        commands: Vec::new(),
        input: InputState::with_text("nvim".to_string()),
        mode: LaunchConfigurationListEditorMode::Edit,
        selected_index: 0,
    };
    let mut empty_add = LaunchConfigurationListEditorState {
        commands: Vec::new(),
        input: InputState::default(),
        mode: LaunchConfigurationListEditorMode::Add,
        selected_index: 0,
    };

    // Act
    let edit_operation = apply_launch_configuration_input(&mut empty_edit);
    let add_operation = apply_launch_configuration_input(&mut empty_add);

    // Assert
    assert_eq!(
        edit_operation,
        SettingsOperation::LaunchConfiguration("nvim".to_string())
    );
    assert_eq!(
        add_operation,
        SettingsOperation::LaunchConfiguration(String::new())
    );
}

#[test]
fn previous_launch_selection_wraps_and_all_row_options_are_available() {
    // Arrange
    let view = test_settings_view("");
    let mut editor = LaunchConfigurationListEditorState {
        commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
        input: InputState::default(),
        mode: LaunchConfigurationListEditorMode::Browse,
        selected_index: 0,
    };

    // Act
    move_launch_configuration_list_editor_selection(&mut editor, false);
    let fast_options = selector_options_for_row(&view, SettingRow::DefaultFastModel);
    let launch_options = selector_options_for_row(&view, SettingRow::LaunchConfiguration);
    let parallelism_options = selector_options_for_row(&view, SettingRow::OrchestrationParallelism);
    let research_options =
        selector_options_for_row(&view, SettingRow::AutoApproveOrchestrationResearch);

    // Assert
    assert_eq!(editor.selected_index, 1);
    assert_eq!(fast_options.len(), view.available_model_selections.len());
    assert_eq!(
        reasoning_selector_option_labels().len(),
        ReasoningLevel::ALL.len()
    );
    assert_eq!(speed_selector_option_labels().len(), SpeedMode::ALL.len());
    assert_eq!(
        launch_options,
        [] as [crate::presentation::setting::SettingSelectorOption; 0]
    );
    assert_eq!(
        parallelism_options.len(),
        usize::from(MAX_ORCHESTRATION_PARALLELISM)
    );
    assert!(parallelism_options[2].is_current_for(&view, SettingRow::OrchestrationParallelism));
    assert_eq!(
        research_options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        ["Disabled", "Enabled"]
    );
    assert!(
        research_options[1].is_current_for(&view, SettingRow::AutoApproveOrchestrationResearch)
    );
    assert_eq!(
        settings_operation_for_primary_selector(
            SettingRow::OrchestrationParallelism,
            SettingSelectorValue::Parallelism(4),
        ),
        Some(SettingsOperation::OrchestrationParallelism(4))
    );
    assert_eq!(
        settings_operation_for_primary_selector(
            SettingRow::AutoApproveOrchestrationResearch,
            SettingSelectorValue::Bool(false),
        ),
        Some(SettingsOperation::AutoApproveOrchestrationResearch(false))
    );
}

#[test]
fn last_used_speed_capable_model_value_includes_speed() {
    // Arrange
    let selection = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol);

    // Act
    let display_value =
        display_last_used_model_value(selection, ReasoningLevel::High, SpeedMode::Fast);

    // Assert
    assert_eq!(display_value, "Last used model as default [high, Fast]");
}

#[test]
fn selector_snapshot_separates_model_reasoning_and_speed() {
    // Arrange
    let view = test_settings_view("");
    let mut model_state = SettingsPresentationState::default();
    select_row(&mut model_state, &view, 3);
    let _ = model_state.apply(&view, SettingsAction::Activate);
    let model_footer_hint = model_state.footer_hint();
    let mut theme_state = SettingsPresentationState::default();
    let _ = theme_state.apply(&view, SettingsAction::Activate);

    // Act
    let model_dropdown = model_state
        .snapshot(&view)
        .selector_dropdown
        .expect("smart model selector should be open");
    let model_operation = model_state.apply(&view, SettingsAction::Confirm);
    let reasoning_dropdown = model_state
        .snapshot(&view)
        .selector_dropdown
        .expect("reasoning selector should be open");
    let reasoning_footer_hint = model_state.footer_hint();
    let mut speed_state = SettingsPresentationState::default();
    select_row(&mut speed_state, &view, 4);
    let _ = speed_state.apply(&view, SettingsAction::Activate);
    let _ = speed_state.apply(&view, SettingsAction::Confirm);
    let speed_capable_reasoning_footer_hint = speed_state.footer_hint();
    let _ = speed_state.apply(&view, SettingsAction::Confirm);
    let speed_next_operation = speed_state.apply(&view, SettingsAction::Next);
    let speed_previous_operation = speed_state.apply(&view, SettingsAction::Previous);
    let speed_dropdown = speed_state
        .snapshot(&view)
        .selector_dropdown
        .expect("speed selector should be open");
    let speed_footer_hint = speed_state.footer_hint();
    let theme_snapshot = theme_state.snapshot(&view);
    let theme_dropdown = theme_snapshot
        .selector_dropdown
        .expect("theme selector should be open");

    // Assert
    assert_eq!(model_operation, None);
    assert_eq!(model_dropdown.title, "Select model");
    assert_eq!(
        model_footer_hint,
        "Selecting model: j/k move, Enter continue, Esc/q close"
    );
    assert_eq!(
        model_dropdown.options[model_dropdown.selected_index].label,
        display_model_selector_value(view.default_smart_selection)
    );
    assert_eq!(
        model_dropdown.options.len(),
        view.available_model_selections.len() + 1
    );
    assert_eq!(reasoning_dropdown.title, "Select reasoning level");
    assert_eq!(
        reasoning_footer_hint,
        "Selecting reasoning: j/k move, Enter save, Esc/q close"
    );
    assert_eq!(
        reasoning_dropdown.options[reasoning_dropdown.selected_index].label,
        ReasoningLevel::High.as_str()
    );
    assert_eq!(
        reasoning_dropdown
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        ReasoningLevel::ALL
            .iter()
            .map(|reasoning_level| reasoning_level.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(speed_dropdown.title, "Select response speed");
    assert_eq!(speed_next_operation, None);
    assert_eq!(speed_previous_operation, None);
    assert_eq!(
        speed_capable_reasoning_footer_hint,
        "Selecting reasoning: j/k move, Enter continue, Esc/q close"
    );
    assert_eq!(
        speed_dropdown.options[speed_dropdown.selected_index].label,
        SpeedMode::Fast.name()
    );
    assert_eq!(
        speed_dropdown
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        SpeedMode::ALL
            .iter()
            .map(|speed_mode| speed_mode.name())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        speed_footer_hint,
        "Selecting speed: j/k move, Enter save, Esc/q close"
    );
    assert_eq!(theme_dropdown.title, "Select setting value");
}
