//! Frontend-neutral settings screen state and actions.

use crate::domain::agent::{AgentSelection, ReasoningLevel};
use crate::domain::input::{InputCommand, InputState};
use crate::domain::selection::SelectionState;
use crate::domain::theme::ColorTheme;

/// Immutable setting values and available choices required by the settings
/// screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettingsView {
    pub(crate) available_model_selections: Vec<AgentSelection>,
    pub(crate) default_fast_selection: AgentSelection,
    pub(crate) default_review_selection: AgentSelection,
    pub(crate) default_smart_selection: AgentSelection,
    pub(crate) include_coauthored_by_agentty: bool,
    pub(crate) launch_configuration: String,
    pub(crate) reasoning_level: ReasoningLevel,
    pub(crate) theme: ColorTheme,
    pub(crate) use_last_used_model_as_default: bool,
}

/// One persistence operation requested by the settings screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SettingsOperation {
    DefaultFastSelection(AgentSelection),
    DefaultReviewSelection(AgentSelection),
    DefaultSmartSelection {
        selection: AgentSelection,
        use_last_used_model_as_default: bool,
    },
    IncludeCoauthoredByAgentty(bool),
    LaunchConfiguration(String),
    ReasoningLevel(ReasoningLevel),
    Theme(ColorTheme),
}

/// A key-independent action supported by the settings screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SettingsAction {
    Activate,
    Cancel,
    Confirm,
    DeleteLaunchConfiguration,
    EditLaunchConfiguration,
    Input(InputCommand),
    MoveLaunchConfigurationDown,
    MoveLaunchConfigurationUp,
    Next,
    Previous,
    StartAddingLaunchConfiguration,
}

/// Render-ready option for an open settings selector dropdown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettingsSelectorDropdownOption {
    pub label: String,
}

/// Render-ready snapshot for the currently open settings selector dropdown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettingsSelectorDropdown {
    pub options: Vec<SettingsSelectorDropdownOption>,
    pub row_index: usize,
    pub selected_index: usize,
}

/// Active interaction mode for the `Launch Configurations` list editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchConfigurationListEditorMode {
    Add,
    Browse,
    Edit,
}

/// Render-ready snapshot for the `Launch Configurations` list editor overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchConfigurationListEditorSnapshot {
    pub commands: Vec<String>,
    pub input: Option<InputState>,
    pub mode: LaunchConfigurationListEditorMode,
    pub selected_index: usize,
}

/// Immutable data required to render one settings screen frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettingsScreenSnapshot {
    pub(crate) footer_hint: &'static str,
    pub(crate) global_rows: Vec<(&'static str, String)>,
    pub(crate) launch_configuration_list_editor: Option<LaunchConfigurationListEditorSnapshot>,
    pub(crate) project_rows: Vec<(&'static str, String)>,
    pub(crate) selected_row_index: Option<usize>,
    pub(crate) selector_dropdown: Option<SettingsSelectorDropdown>,
}

/// Presentation-owned interaction state for the settings tab.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettingsPresentationState {
    launch_configuration_list_editor: Option<LaunchConfigurationListEditorState>,
    selector_dropdown: Option<SelectorDropdownState>,
    table_state: SelectionState,
}

impl Default for SettingsPresentationState {
    fn default() -> Self {
        let mut table_state = SelectionState::default();
        table_state.select(Some(0));

        Self {
            launch_configuration_list_editor: None,
            selector_dropdown: None,
            table_state,
        }
    }
}

impl SettingsPresentationState {
    /// Applies one semantic settings action and returns the persistence
    /// request, if the action changed a setting value.
    pub(crate) fn apply(
        &mut self,
        view: &SettingsView,
        action: SettingsAction,
    ) -> Option<SettingsOperation> {
        match action {
            SettingsAction::Activate => self.activate(view),
            SettingsAction::Cancel => self.cancel(),
            SettingsAction::Confirm => self.confirm(view),
            SettingsAction::DeleteLaunchConfiguration => self.delete_launch_configuration(),
            SettingsAction::EditLaunchConfiguration => self.edit_launch_configuration(),
            SettingsAction::Input(command) => {
                self.apply_launch_configuration_input(command);

                None
            }
            SettingsAction::MoveLaunchConfigurationDown => {
                self.move_launch_configuration(LaunchConfigurationReorderDirection::Down)
            }
            SettingsAction::MoveLaunchConfigurationUp => {
                self.move_launch_configuration(LaunchConfigurationReorderDirection::Up)
            }
            SettingsAction::Next => {
                self.next(view);

                None
            }
            SettingsAction::Previous => {
                self.previous(view);

                None
            }
            SettingsAction::StartAddingLaunchConfiguration => {
                self.start_adding_launch_configuration();

                None
            }
        }
    }

    /// Returns whether the launch-configuration editor is open.
    pub(crate) fn is_launch_configuration_list_editor_open(&self) -> bool {
        self.launch_configuration_list_editor.is_some()
    }

    /// Returns whether the launch-configuration editor accepts text input.
    pub(crate) fn is_launch_configuration_list_editor_input_active(&self) -> bool {
        self.launch_configuration_list_editor
            .as_ref()
            .is_some_and(LaunchConfigurationListEditorState::is_input_mode)
    }

    /// Returns whether a setting selector is open.
    pub(crate) fn is_selector_dropdown_open(&self) -> bool {
        self.selector_dropdown.is_some()
    }

    /// Creates the immutable settings-screen projection consumed by the UI.
    pub(crate) fn snapshot(&self, view: &SettingsView) -> SettingsScreenSnapshot {
        SettingsScreenSnapshot {
            footer_hint: self.footer_hint(),
            global_rows: SettingRow::GLOBAL
                .iter()
                .map(|row| (row.label(), display_value_for_row(view, *row)))
                .collect(),
            launch_configuration_list_editor: self.launch_configuration_list_editor(),
            project_rows: SettingRow::PROJECT
                .iter()
                .map(|row| (row.label(), display_value_for_row(view, *row)))
                .collect(),
            selected_row_index: self.table_state.selected(),
            selector_dropdown: self.selector_dropdown(view),
        }
    }

    fn activate(&mut self, view: &SettingsView) -> Option<SettingsOperation> {
        if self.is_launch_configuration_list_editor_open() {
            return self.edit_launch_configuration();
        }

        if self.is_selector_dropdown_open() {
            return self.select_selector_dropdown_option(view);
        }

        match self.selected_row().control() {
            SettingControl::CommandList => {
                self.launch_configuration_list_editor = Some(
                    LaunchConfigurationListEditorState::from_launch_configuration(
                        view.launch_configuration.as_str(),
                    ),
                );
            }
            SettingControl::Selector => self.open_selector_dropdown(view, self.selected_row()),
        }

        None
    }

    fn cancel(&mut self) -> Option<SettingsOperation> {
        if self.is_selector_dropdown_open() {
            self.selector_dropdown = None;
        } else if self.is_launch_configuration_list_editor_input_active() {
            if let Some(editor) = &mut self.launch_configuration_list_editor {
                editor.input = InputState::default();
                editor.mode = LaunchConfigurationListEditorMode::Browse;
            }
        } else {
            self.launch_configuration_list_editor = None;
        }

        None
    }

    fn confirm(&mut self, view: &SettingsView) -> Option<SettingsOperation> {
        if self.is_selector_dropdown_open() {
            return self.select_selector_dropdown_option(view);
        }

        let Some(editor) = &mut self.launch_configuration_list_editor else {
            return None;
        };

        if editor.is_input_mode() {
            return Some(apply_launch_configuration_input(editor));
        }

        self.edit_launch_configuration()
    }

    fn delete_launch_configuration(&mut self) -> Option<SettingsOperation> {
        let editor = self.launch_configuration_list_editor.as_mut()?;
        if editor.commands.is_empty() || editor.is_input_mode() {
            return None;
        }

        editor.commands.remove(editor.selected_index());
        editor.clamp_selected_index();

        Some(SettingsOperation::LaunchConfiguration(
            join_launch_configurations(&editor.commands),
        ))
    }

    fn edit_launch_configuration(&mut self) -> Option<SettingsOperation> {
        let Some(editor) = &mut self.launch_configuration_list_editor else {
            return None;
        };

        if editor.commands.is_empty() {
            editor.input = InputState::default();
            editor.mode = LaunchConfigurationListEditorMode::Add;

            return None;
        }

        let selected_index = editor.selected_index();
        editor.input = InputState::with_text(editor.commands[selected_index].clone());
        editor.mode = LaunchConfigurationListEditorMode::Edit;

        None
    }

    fn apply_launch_configuration_input(&mut self, command: InputCommand) {
        let Some(editor) = &mut self.launch_configuration_list_editor else {
            return;
        };

        if editor.is_input_mode() {
            editor.input.apply(command);
        }
    }

    fn launch_configuration_list_editor(&self) -> Option<LaunchConfigurationListEditorSnapshot> {
        let editor = self.launch_configuration_list_editor.as_ref()?;

        Some(LaunchConfigurationListEditorSnapshot {
            commands: editor.commands.clone(),
            input: editor.is_input_mode().then(|| editor.input.clone()),
            mode: editor.mode,
            selected_index: editor.selected_index(),
        })
    }

    fn move_launch_configuration(
        &mut self,
        direction: LaunchConfigurationReorderDirection,
    ) -> Option<SettingsOperation> {
        let editor = self.launch_configuration_list_editor.as_mut()?;
        if editor.commands.len() < 2 || editor.is_input_mode() {
            return None;
        }

        let selected_index = editor.selected_index();
        let next_index = match direction {
            LaunchConfigurationReorderDirection::Down
                if selected_index + 1 < editor.commands.len() =>
            {
                selected_index + 1
            }
            LaunchConfigurationReorderDirection::Up if selected_index > 0 => selected_index - 1,
            _ => return None,
        };
        editor.commands.swap(selected_index, next_index);
        editor.selected_index = next_index;

        Some(SettingsOperation::LaunchConfiguration(
            join_launch_configurations(&editor.commands),
        ))
    }

    fn next(&mut self, view: &SettingsView) {
        if let Some(selector_dropdown) = self.selector_dropdown {
            self.move_selector_dropdown_option(view, selector_dropdown, true);
        } else if let Some(editor) = &mut self.launch_configuration_list_editor {
            move_launch_configuration_list_editor_selection(editor, true);
        } else {
            let selected_index = self.selected_row_index();
            self.table_state
                .select(Some((selected_index + 1) % SettingRow::ROW_COUNT));
        }
    }

    fn previous(&mut self, view: &SettingsView) {
        if let Some(selector_dropdown) = self.selector_dropdown {
            self.move_selector_dropdown_option(view, selector_dropdown, false);
        } else if let Some(editor) = &mut self.launch_configuration_list_editor {
            move_launch_configuration_list_editor_selection(editor, false);
        } else {
            let selected_index = self.selected_row_index();
            let previous_index = selected_index
                .checked_sub(1)
                .unwrap_or(SettingRow::ROW_COUNT - 1);
            self.table_state.select(Some(previous_index));
        }
    }

    fn open_selector_dropdown(&mut self, view: &SettingsView, row: SettingRow) {
        let options = selector_options_for_row(view, row);
        if options.is_empty() {
            return;
        }

        let selected_index = options
            .iter()
            .position(|option| option.is_current_for(view, row))
            .unwrap_or_default();
        self.selector_dropdown = Some(SelectorDropdownState {
            row,
            selected_index,
        });
    }

    fn move_selector_dropdown_option(
        &mut self,
        view: &SettingsView,
        selector_dropdown: SelectorDropdownState,
        is_next: bool,
    ) {
        let option_count = selector_options_for_row(view, selector_dropdown.row).len();
        if option_count == 0 {
            self.selector_dropdown = None;

            return;
        }

        let selected_index = if is_next {
            (selector_dropdown.selected_index + 1) % option_count
        } else {
            selector_dropdown
                .selected_index
                .checked_sub(1)
                .unwrap_or(option_count - 1)
        };
        self.selector_dropdown = Some(SelectorDropdownState {
            row: selector_dropdown.row,
            selected_index,
        });
    }

    fn select_selector_dropdown_option(
        &mut self,
        view: &SettingsView,
    ) -> Option<SettingsOperation> {
        let selector_dropdown = self.selector_dropdown?;
        let options = selector_options_for_row(view, selector_dropdown.row);
        let value = options
            .get(
                selector_dropdown
                    .selected_index
                    .min(options.len().saturating_sub(1)),
            )?
            .value;
        self.selector_dropdown = None;

        settings_operation_for_selector(view, selector_dropdown.row, value)
    }

    fn selected_row_index(&self) -> usize {
        self.table_state
            .selected()
            .unwrap_or_default()
            .min(SettingRow::ROW_COUNT - 1)
    }

    fn selected_row(&self) -> SettingRow {
        SettingRow::from_index(self.selected_row_index())
    }

    fn selector_dropdown(&self, view: &SettingsView) -> Option<SettingsSelectorDropdown> {
        let selector_dropdown = self.selector_dropdown?;
        let options = selector_options_for_row(view, selector_dropdown.row);
        let selected_index = selector_dropdown
            .selected_index
            .min(options.len().saturating_sub(1));

        Some(SettingsSelectorDropdown {
            options: options
                .into_iter()
                .map(|option| SettingsSelectorDropdownOption {
                    label: option.label,
                })
                .collect(),
            row_index: selector_dropdown.row.table_index(),
            selected_index,
        })
    }

    fn start_adding_launch_configuration(&mut self) {
        let Some(editor) = &mut self.launch_configuration_list_editor else {
            return;
        };

        editor.input = InputState::default();
        editor.mode = LaunchConfigurationListEditorMode::Add;
    }

    fn footer_hint(&self) -> &'static str {
        if self.is_launch_configuration_list_editor_input_active() {
            "Launch Configurations: type a command, Enter save, Esc cancel"
        } else if self.is_launch_configuration_list_editor_open() {
            "Launch Configurations: j/k move, a add, e/Enter edit, d delete, J/K reorder, Esc/q \
             close"
        } else if self.is_selector_dropdown_open() {
            "Selecting setting value: j/k move, Enter select, Esc/q close"
        } else {
            "Settings: Enter opens selectors or command editor"
        }
    }
}

fn settings_operation_for_selector(
    view: &SettingsView,
    row: SettingRow,
    value: SettingSelectorValue,
) -> Option<SettingsOperation> {
    match (row, value) {
        (SettingRow::ReasoningLevel, SettingSelectorValue::ReasoningLevel(value)) => {
            Some(SettingsOperation::ReasoningLevel(value))
        }
        (SettingRow::DefaultSmartModel, SettingSelectorValue::LastUsedModel) => {
            Some(SettingsOperation::DefaultSmartSelection {
                selection: view.default_smart_selection,
                use_last_used_model_as_default: true,
            })
        }
        (SettingRow::DefaultSmartModel, SettingSelectorValue::ModelSelection(selection)) => {
            Some(SettingsOperation::DefaultSmartSelection {
                selection,
                use_last_used_model_as_default: false,
            })
        }
        (SettingRow::DefaultFastModel, SettingSelectorValue::ModelSelection(selection)) => {
            Some(SettingsOperation::DefaultFastSelection(selection))
        }
        (SettingRow::DefaultReviewModel, SettingSelectorValue::ModelSelection(selection)) => {
            Some(SettingsOperation::DefaultReviewSelection(selection))
        }
        (SettingRow::IncludeCoauthoredByAgentty, SettingSelectorValue::Bool(value)) => {
            Some(SettingsOperation::IncludeCoauthoredByAgentty(value))
        }
        (SettingRow::Theme, SettingSelectorValue::Theme(value)) => {
            Some(SettingsOperation::Theme(value))
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingControl {
    CommandList,
    Selector,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingRow {
    ReasoningLevel,
    DefaultSmartModel,
    DefaultFastModel,
    DefaultReviewModel,
    IncludeCoauthoredByAgentty,
    LaunchConfiguration,
    Theme,
}

impl SettingRow {
    const ALL: [Self; 7] = [
        Self::Theme,
        Self::ReasoningLevel,
        Self::DefaultSmartModel,
        Self::DefaultFastModel,
        Self::DefaultReviewModel,
        Self::IncludeCoauthoredByAgentty,
        Self::LaunchConfiguration,
    ];
    const GLOBAL: [Self; 1] = [Self::Theme];
    const PROJECT: [Self; 6] = [
        Self::ReasoningLevel,
        Self::DefaultSmartModel,
        Self::DefaultFastModel,
        Self::DefaultReviewModel,
        Self::IncludeCoauthoredByAgentty,
        Self::LaunchConfiguration,
    ];
    const ROW_COUNT: usize = Self::ALL.len();

    fn from_index(index: usize) -> Self {
        Self::ALL
            .get(index)
            .copied()
            .unwrap_or(Self::ReasoningLevel)
    }

    fn control(self) -> SettingControl {
        match self {
            Self::LaunchConfiguration => SettingControl::CommandList,
            _ => SettingControl::Selector,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ReasoningLevel => "Default Reasoning Level",
            Self::DefaultSmartModel => "Default Smart Model",
            Self::DefaultFastModel => "Default Fast Model",
            Self::DefaultReviewModel => "Default Review Model",
            Self::IncludeCoauthoredByAgentty => "Coauthored by Agentty",
            Self::LaunchConfiguration => "Launch Configurations",
            Self::Theme => "Theme",
        }
    }

    fn table_index(self) -> usize {
        Self::ALL
            .iter()
            .position(|row| *row == self)
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SelectorDropdownState {
    row: SettingRow,
    selected_index: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LaunchConfigurationListEditorState {
    commands: Vec<String>,
    input: InputState,
    mode: LaunchConfigurationListEditorMode,
    selected_index: usize,
}

impl LaunchConfigurationListEditorState {
    fn from_launch_configuration(launch_configuration: &str) -> Self {
        Self {
            commands: parse_launch_configurations(launch_configuration),
            input: InputState::default(),
            mode: LaunchConfigurationListEditorMode::Browse,
            selected_index: 0,
        }
    }

    fn is_input_mode(&self) -> bool {
        matches!(
            self.mode,
            LaunchConfigurationListEditorMode::Add | LaunchConfigurationListEditorMode::Edit
        )
    }

    fn selected_index(&self) -> usize {
        self.selected_index
            .min(self.commands.len().saturating_sub(1))
    }

    fn clamp_selected_index(&mut self) {
        self.selected_index = self.selected_index();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LaunchConfigurationReorderDirection {
    Down,
    Up,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SettingSelectorOption {
    label: String,
    value: SettingSelectorValue,
}

impl SettingSelectorOption {
    fn is_current_for(&self, view: &SettingsView, row: SettingRow) -> bool {
        match (row, self.value) {
            (SettingRow::ReasoningLevel, SettingSelectorValue::ReasoningLevel(value)) => {
                view.reasoning_level == value
            }
            (SettingRow::DefaultSmartModel, SettingSelectorValue::LastUsedModel) => {
                view.use_last_used_model_as_default
            }
            (SettingRow::DefaultSmartModel, SettingSelectorValue::ModelSelection(value)) => {
                !view.use_last_used_model_as_default && view.default_smart_selection == value
            }
            (SettingRow::DefaultFastModel, SettingSelectorValue::ModelSelection(value)) => {
                view.default_fast_selection == value
            }
            (SettingRow::DefaultReviewModel, SettingSelectorValue::ModelSelection(value)) => {
                view.default_review_selection == value
            }
            (SettingRow::IncludeCoauthoredByAgentty, SettingSelectorValue::Bool(value)) => {
                view.include_coauthored_by_agentty == value
            }
            (SettingRow::Theme, SettingSelectorValue::Theme(value)) => view.theme == value,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingSelectorValue {
    Bool(bool),
    LastUsedModel,
    ModelSelection(AgentSelection),
    ReasoningLevel(ReasoningLevel),
    Theme(ColorTheme),
}

fn apply_launch_configuration_input(
    editor: &mut LaunchConfigurationListEditorState,
) -> SettingsOperation {
    let command = editor.input.text().trim().to_string();
    match editor.mode {
        LaunchConfigurationListEditorMode::Add if !command.is_empty() => {
            editor.commands.push(command);
            editor.selected_index = editor.commands.len().saturating_sub(1);
        }
        LaunchConfigurationListEditorMode::Edit
            if editor.commands.is_empty() && !command.is_empty() =>
        {
            editor.commands.push(command);
            editor.selected_index = 0;
        }
        LaunchConfigurationListEditorMode::Edit if !editor.commands.is_empty() => {
            let selected_index = editor.selected_index();
            if command.is_empty() {
                editor.commands.remove(selected_index);
                editor.clamp_selected_index();
            } else {
                editor.commands[selected_index] = command;
            }
        }
        _ => {}
    }
    editor.input = InputState::default();
    editor.mode = LaunchConfigurationListEditorMode::Browse;

    SettingsOperation::LaunchConfiguration(join_launch_configurations(&editor.commands))
}

fn move_launch_configuration_list_editor_selection(
    editor: &mut LaunchConfigurationListEditorState,
    is_next: bool,
) {
    if editor.commands.is_empty() || editor.is_input_mode() {
        return;
    }

    editor.selected_index = if is_next {
        (editor.selected_index + 1) % editor.commands.len()
    } else {
        editor
            .selected_index
            .checked_sub(1)
            .unwrap_or(editor.commands.len() - 1)
    };
}

fn selector_options_for_row(view: &SettingsView, row: SettingRow) -> Vec<SettingSelectorOption> {
    match row {
        SettingRow::ReasoningLevel => ReasoningLevel::ALL
            .iter()
            .copied()
            .map(|value| SettingSelectorOption {
                label: value.codex().to_string(),
                value: SettingSelectorValue::ReasoningLevel(value),
            })
            .collect(),
        SettingRow::DefaultSmartModel => {
            let mut options = model_selector_options(view);
            options.push(SettingSelectorOption {
                label: "Last used model as default".to_string(),
                value: SettingSelectorValue::LastUsedModel,
            });
            options
        }
        SettingRow::DefaultFastModel | SettingRow::DefaultReviewModel => {
            model_selector_options(view)
        }
        SettingRow::IncludeCoauthoredByAgentty => vec![
            SettingSelectorOption {
                label: bool_setting_display(false),
                value: SettingSelectorValue::Bool(false),
            },
            SettingSelectorOption {
                label: bool_setting_display(true),
                value: SettingSelectorValue::Bool(true),
            },
        ],
        SettingRow::LaunchConfiguration => Vec::new(),
        SettingRow::Theme => ColorTheme::ALL
            .iter()
            .copied()
            .map(|value| SettingSelectorOption {
                label: value.label().to_string(),
                value: SettingSelectorValue::Theme(value),
            })
            .collect(),
    }
}

fn model_selector_options(view: &SettingsView) -> Vec<SettingSelectorOption> {
    view.available_model_selections
        .iter()
        .copied()
        .map(|selection| SettingSelectorOption {
            label: display_model_selector_value(selection),
            value: SettingSelectorValue::ModelSelection(selection),
        })
        .collect()
}

fn display_value_for_row(view: &SettingsView, row: SettingRow) -> String {
    match row {
        SettingRow::ReasoningLevel => view.reasoning_level.codex().to_string(),
        SettingRow::DefaultSmartModel if view.use_last_used_model_as_default => {
            "Last used model as default".to_string()
        }
        SettingRow::DefaultSmartModel => display_model_selector_value(view.default_smart_selection),
        SettingRow::DefaultFastModel => display_model_selector_value(view.default_fast_selection),
        SettingRow::DefaultReviewModel => {
            display_model_selector_value(view.default_review_selection)
        }
        SettingRow::IncludeCoauthoredByAgentty => {
            bool_setting_display(view.include_coauthored_by_agentty)
        }
        SettingRow::LaunchConfiguration => {
            display_launch_configuration_summary(&view.launch_configuration)
        }
        SettingRow::Theme => view.theme.label().to_string(),
    }
}

fn bool_setting_display(value: bool) -> String {
    if value {
        "Enabled".to_string()
    } else {
        "Disabled".to_string()
    }
}

fn display_launch_configuration_summary(value: &str) -> String {
    let commands = parse_launch_configurations(value);
    let Some(first_command) = commands.first() else {
        return "(none)".to_string();
    };
    if commands.len() == 1 {
        return first_command.clone();
    }

    format!("{} (+{} more)", first_command, commands.len() - 1)
}

fn display_model_selector_value(selection: AgentSelection) -> String {
    format!("{}/{}", selection.kind(), selection.model().as_str())
}

fn join_launch_configurations(commands: &[String]) -> String {
    commands
        .iter()
        .map(|command| command.trim())
        .filter(|command| !command.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_launch_configurations(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::agent::{AgentKind, AgentModel};

    fn test_settings_view(launch_configuration: &str) -> SettingsView {
        let smart_selection = AgentSelection::new(
            AgentKind::Antigravity,
            AgentKind::Antigravity.default_model(),
        );

        SettingsView {
            available_model_selections: vec![
                smart_selection,
                AgentSelection::new(AgentKind::Codex, AgentModel::Gpt55),
                AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus48),
            ],
            default_fast_selection: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt55),
            default_review_selection: AgentSelection::new(
                AgentKind::Claude,
                AgentModel::ClaudeOpus48,
            ),
            default_smart_selection: smart_selection,
            include_coauthored_by_agentty: false,
            launch_configuration: launch_configuration.to_string(),
            reasoning_level: ReasoningLevel::High,
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

    #[test]
    fn activate_reuses_open_selector_and_launch_editor() {
        // Arrange
        let empty_view = test_settings_view("");
        let mut selector_state = SettingsPresentationState::default();
        let mut editor_state = SettingsPresentationState::default();
        select_row(&mut editor_state, &empty_view, 6);

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
        select_row(&mut state, &view, 6);
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
        select_row(&mut one_command_state, &one_command_view, 6);
        let _ = one_command_state.apply(&one_command_view, SettingsAction::Activate);
        let two_command_view = test_settings_view("cargo test\nnpm run dev");
        let mut two_command_state = SettingsPresentationState::default();
        select_row(&mut two_command_state, &two_command_view, 6);
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
    fn selectors_cover_reasoning_fast_review_and_invalid_pairs() {
        // Arrange
        let view = test_settings_view("");

        // Act
        let operations = [
            (1, SettingsOperation::ReasoningLevel(ReasoningLevel::High)),
            (
                3,
                SettingsOperation::DefaultFastSelection(view.default_fast_selection),
            ),
            (
                4,
                SettingsOperation::DefaultReviewSelection(view.default_review_selection),
            ),
        ]
        .map(|(row_index, expected_operation)| {
            let mut state = SettingsPresentationState::default();
            select_row(&mut state, &view, row_index);
            let _ = state.apply(&view, SettingsAction::Activate);

            (
                state.apply(&view, SettingsAction::Confirm),
                expected_operation,
            )
        });
        let mismatched_option = SettingSelectorOption {
            label: "Enabled".to_string(),
            value: SettingSelectorValue::Bool(true),
        };
        let is_mismatched_current = mismatched_option.is_current_for(&view, SettingRow::Theme);
        let invalid_operation = settings_operation_for_selector(
            &view,
            SettingRow::Theme,
            SettingSelectorValue::Bool(true),
        );

        // Assert
        for (operation, expected_operation) in operations {
            assert_eq!(operation, Some(expected_operation));
        }
        assert!(!is_mismatched_current);
        assert_eq!(invalid_operation, None);
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
        let reasoning_options = selector_options_for_row(&view, SettingRow::ReasoningLevel);
        let fast_options = selector_options_for_row(&view, SettingRow::DefaultFastModel);
        let launch_options = selector_options_for_row(&view, SettingRow::LaunchConfiguration);

        // Assert
        assert_eq!(editor.selected_index, 1);
        assert_eq!(reasoning_options.len(), ReasoningLevel::ALL.len());
        assert_eq!(fast_options.len(), view.available_model_selections.len());
        assert!(launch_options.is_empty());
    }
}
