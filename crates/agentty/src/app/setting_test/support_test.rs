use std::path::PathBuf;
use std::sync::Arc;

use ag_agent::{AgentSelectionMetadata, MockAppServerClient};
use ag_forge as forge;
use ag_git as git;
use tokio::sync::mpsc;

use super::super::{
    ModelSelectorOption, SettingsManager, parse_launch_configurations, selectable_model_options,
};
use crate::app::AppServices;
use crate::db::AppRepositories;
use crate::domain::agent::{AgentKind, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode};
use crate::domain::input::InputCommand;
use crate::domain::setting::{
    DEFAULT_AUTO_APPROVE_ORCHESTRATION_RESEARCH, DEFAULT_ORCHESTRATION_PARALLELISM, SettingName,
};
use crate::domain::theme::ColorTheme;
use crate::infra::fs;
use crate::presentation::setting::{
    LaunchConfigurationListEditorSnapshot, SettingsAction, SettingsOperation,
    SettingsPresentationState, SettingsSelectorDropdown, SettingsView,
};

/// Builds app services backed by an in-memory database for settings tests.
pub(super) async fn test_services() -> (AppServices, i64) {
    test_services_with_available_agent_kinds(AgentKind::ALL.to_vec()).await
}

/// Builds settings test services with a caller-provided provider
/// availability snapshot.
pub(super) async fn test_services_with_available_agent_kinds(
    available_agent_kinds: Vec<AgentKind>,
) -> (AppServices, i64) {
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to create project");
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let services = AppServices::new_with_agent_clis(
        PathBuf::from("/tmp/agentty-settings-tests"),
        Arc::new(crate::infra::clock::RealClock),
        event_tx,
        crate::app::service::AppServiceDeps {
            app_server_client_override: Some(Arc::new(MockAppServerClient::new())),
            available_agent_kinds: available_agent_kinds.clone(),
            clipboard_image_client_override: None,
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(git::MockGitClient::new()),
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: database.clone(),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        },
        crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds),
    );

    (services, project_id)
}

/// Test-local settings screen harness that composes production value and
/// presentation boundaries without extending `SettingsManager`.
pub(super) struct SettingsTestHarness {
    pub(super) manager: Option<SettingsManager>,
    pub(super) presentation: SettingsPresentationState,
    pub(super) view: SettingsView,
}

impl SettingsTestHarness {
    pub(super) fn new() -> Self {
        let default_selection = AgentSelection::new(
            AgentKind::Antigravity,
            AgentKind::Antigravity.default_model(),
        );

        Self {
            manager: None,
            presentation: SettingsPresentationState::default(),
            view: SettingsView {
                available_model_selections: selectable_model_options(AgentKind::ALL)
                    .into_iter()
                    .map(ModelSelectorOption::selection)
                    .collect(),
                auto_approve_orchestration_research: DEFAULT_AUTO_APPROVE_ORCHESTRATION_RESEARCH,
                default_fast_reasoning_level: ReasoningLevel::Low,
                default_fast_selection: default_selection,
                default_fast_speed_mode: SpeedMode::Normal,
                default_review_reasoning_level: ReasoningLevel::XHigh,
                default_review_selection: default_selection,
                default_review_speed_mode: SpeedMode::Normal,
                default_response_style: ResponseStyle::Balanced,
                default_smart_reasoning_level: ReasoningLevel::High,
                default_smart_selection: default_selection,
                default_smart_speed_mode: SpeedMode::Normal,
                include_coauthored_by_agentty: false,
                launch_configuration: String::new(),
                orchestration_parallelism: DEFAULT_ORCHESTRATION_PARALLELISM,
                theme: ColorTheme::Current,
                use_last_used_model_as_default: false,
            },
        }
    }

    pub(super) fn from_manager(manager: SettingsManager) -> Self {
        Self {
            view: manager.view(),
            manager: Some(manager),
            presentation: SettingsPresentationState::default(),
        }
    }

    pub(super) fn apply(&mut self, action: SettingsAction) -> Option<SettingsOperation> {
        self.presentation.apply(&self.view, action)
    }

    pub(super) async fn apply_and_persist(&mut self, action: SettingsAction) {
        let Some(operation) = self.apply(action) else {
            return;
        };

        self.persist_operation(operation).await;
    }

    pub(super) async fn persist_operation(&mut self, operation: SettingsOperation) {
        let manager = self
            .manager
            .as_mut()
            .expect("persisted settings test requires a manager");
        manager.apply_operation(operation).await;
        self.view = manager.view();
    }

    pub(super) fn fixture_view_mut(&mut self) -> &mut SettingsView {
        assert!(
            self.manager.is_none(),
            "persisted settings fixtures must be seeded through repositories"
        );

        &mut self.view
    }

    pub(super) fn settings(&self) -> &SettingsManager {
        self.manager
            .as_ref()
            .expect("test requires repository-backed settings")
    }

    pub(super) fn next(&mut self) {
        let _ = self.apply(SettingsAction::Next);
    }

    pub(super) fn previous(&mut self) {
        let _ = self.apply(SettingsAction::Previous);
    }

    pub(super) fn handle_enter(&mut self) {
        let _ = self.apply(SettingsAction::Activate);
    }

    pub(super) fn is_launch_configuration_list_editor_open(&self) -> bool {
        self.presentation.is_launch_configuration_list_editor_open()
    }

    pub(super) fn is_selector_dropdown_open(&self) -> bool {
        self.presentation.is_selector_dropdown_open()
    }

    pub(super) fn launch_configuration_list_editor(
        &self,
    ) -> Option<LaunchConfigurationListEditorSnapshot> {
        self.presentation
            .snapshot(&self.view)
            .launch_configuration_list_editor
    }

    pub(super) fn launch_configurations(&self) -> Vec<String> {
        parse_launch_configurations(self.view.launch_configuration.as_str())
    }

    pub(super) fn selector_dropdown(&self) -> Option<SettingsSelectorDropdown> {
        self.presentation.snapshot(&self.view).selector_dropdown
    }

    pub(super) fn start_adding_launch_configuration(&mut self) {
        let _ = self.apply(SettingsAction::StartAddingLaunchConfiguration);
    }

    pub(super) fn start_editing_selected_launch_configuration(&mut self) {
        let _ = self.apply(SettingsAction::EditLaunchConfiguration);
    }

    pub(super) fn cancel_launch_configuration_input(&mut self) {
        let _ = self.apply(SettingsAction::Cancel);
    }

    pub(super) fn apply_launch_configuration_input_command(&mut self, command: InputCommand) {
        let _ = self.apply(SettingsAction::Input(command));
    }

    pub(super) fn next_launch_configuration_list_editor_item(&mut self) {
        self.next();
    }

    pub(super) fn next_selector_dropdown_option(&mut self) {
        self.next();
    }

    pub(super) async fn confirm_launch_configuration_input(&mut self) {
        self.apply_and_persist(SettingsAction::Confirm).await;
    }

    pub(super) async fn delete_selected_launch_configuration(&mut self) {
        self.apply_and_persist(SettingsAction::DeleteLaunchConfiguration)
            .await;
    }

    pub(super) async fn move_selected_launch_configuration_down(&mut self) {
        self.apply_and_persist(SettingsAction::MoveLaunchConfigurationDown)
            .await;
    }

    pub(super) async fn move_selected_launch_configuration_up(&mut self) {
        self.apply_and_persist(SettingsAction::MoveLaunchConfigurationUp)
            .await;
    }

    pub(super) async fn select_selector_dropdown_option(&mut self) {
        self.apply_and_persist(SettingsAction::Confirm).await;
    }

    pub(super) fn settings_rows(&self) -> Vec<(&'static str, String)> {
        let snapshot = self.presentation.snapshot(&self.view);

        snapshot
            .global_rows
            .into_iter()
            .chain(snapshot.project_rows)
            .collect()
    }

    pub(super) fn global_settings_rows(&self) -> Vec<(&'static str, String)> {
        self.presentation.snapshot(&self.view).global_rows
    }

    pub(super) fn project_settings_rows(&self) -> Vec<(&'static str, String)> {
        self.presentation.snapshot(&self.view).project_rows
    }

    pub(super) fn footer_hint(&self) -> &'static str {
        self.presentation.snapshot(&self.view).footer_hint
    }
}

/// Selects one settings row through the screen navigation action.
pub(super) fn select_row(manager: &mut SettingsTestHarness, row_index: usize) {
    for _ in 0..row_index {
        manager.next();
    }
}

/// Creates an in-memory settings-screen fixture.
pub(super) fn new_settings_manager() -> SettingsTestHarness {
    SettingsTestHarness::new()
}

/// Loads a settings screen through the production repository boundary.
pub(super) async fn settings_manager(
    services: &AppServices,
    project_id: i64,
) -> SettingsTestHarness {
    let manager = SettingsManager::from_repositories(
        services.db().clone(),
        services.available_agent_kinds(),
        project_id,
    )
    .await;

    SettingsTestHarness::from_manager(manager)
}

pub(super) async fn assert_role_defaults_persisted(
    services: &AppServices,
    project_id: i64,
    setting_names: (SettingName, SettingName, SettingName, SettingName),
    expected: (AgentSelection, ReasoningLevel, SpeedMode),
) {
    let (agent_name, model_name, reasoning_name, speed_name) = setting_names;
    let (selection, reasoning_level, speed_mode) = expected;
    let settings = services.db().settings();

    assert_eq!(
        settings
            .get_project_setting(project_id, agent_name)
            .await
            .expect("failed to load role agent"),
        Some(selection.kind().name().to_string())
    );
    assert_eq!(
        settings
            .get_project_setting(project_id, model_name)
            .await
            .expect("failed to load role model"),
        Some(selection.model().as_str().to_string())
    );
    assert_eq!(
        settings
            .load_project_reasoning_level(project_id, reasoning_name)
            .await
            .expect("failed to load role reasoning level"),
        reasoning_level
    );
    assert_eq!(
        settings
            .load_project_speed_mode(project_id, speed_name)
            .await
            .expect("failed to load role speed mode"),
        speed_mode
    );
}

/// Persists a launch-configuration fixture before loading the production
/// settings boundary.
pub(super) async fn settings_manager_with_launch_configuration(
    services: &AppServices,
    project_id: i64,
    launch_configuration: &str,
) -> SettingsTestHarness {
    services
        .db()
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::LaunchConfiguration,
            launch_configuration,
        )
        .await
        .expect("failed to persist launch-configuration fixture");

    settings_manager(services, project_id).await
}
