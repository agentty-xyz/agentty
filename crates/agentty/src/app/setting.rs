use tracing::warn;

use crate::app::AppServices;
use crate::domain::agent::{
    self, AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata, ReasoningLevel,
};
use crate::domain::setting::SettingName;
use crate::domain::theme::ColorTheme;
use crate::infra::db::AppRepositories;
use crate::presentation::settings::{SettingsOperation, SettingsView};

/// Loads the persisted smart-model default used for new sessions.
///
/// This returns the model from the project-scoped smart agent/model selection
/// and otherwise falls back to `fallback_model`.
pub(crate) async fn load_default_smart_model_setting(
    services: &AppServices,
    project_id: Option<i64>,
    fallback_model: AgentModel,
) -> AgentModel {
    let available_agent_kinds = services.available_agent_kinds();

    load_default_smart_agent_setting(
        services,
        project_id,
        fallback_selection_for_available_model(fallback_model, &available_agent_kinds),
    )
    .await
    .model()
}

/// Loads the persisted smart-model default as an agent/model selection.
///
/// This prefers the project-scoped `DefaultSmartAgent` and
/// `DefaultSmartModel` keys, then falls back to `fallback_selection`. Legacy
/// projects that only have `DefaultSmartModel` still resolve the owning agent
/// from the currently available provider list.
pub(crate) async fn load_default_smart_agent_setting(
    services: &AppServices,
    project_id: Option<i64>,
    fallback_selection: AgentSelection,
) -> AgentSelection {
    let available_agent_kinds = services.available_agent_kinds();
    load_default_smart_agent_selection_from_repositories(
        services.db(),
        project_id,
        fallback_selection,
        &available_agent_kinds,
    )
    .await
}

/// Loads the persisted fast-model default as an agent/model selection.
///
/// This prefers `DefaultFastAgent` and `DefaultFastModel`, then falls back to
/// the resolved smart-model default when the fast-model setting is missing.
pub(crate) async fn load_default_fast_agent_setting(
    services: &AppServices,
    project_id: Option<i64>,
    fallback_selection: AgentSelection,
) -> AgentSelection {
    let available_agent_kinds = services.available_agent_kinds();
    load_default_fast_agent_selection_from_repositories(
        services.db(),
        project_id,
        fallback_selection,
        &available_agent_kinds,
    )
    .await
}

/// Loads the persisted fast-model default from repositories as an agent/model
/// selection.
///
/// This is used by background workflows that only have repository access.
/// Legacy model-only settings still resolve ownership from the fallback
/// selection and available-provider order.
pub(crate) async fn load_default_fast_agent_selection_from_repositories(
    repositories: &AppRepositories,
    project_id: Option<i64>,
    fallback_selection: AgentSelection,
    available_agent_kinds: &[AgentKind],
) -> AgentSelection {
    let fallback_selection = resolve_available_selection(fallback_selection, available_agent_kinds);

    if let Some(selection) = load_model_selection_setting(
        repositories,
        project_id,
        SettingName::DefaultFastAgent,
        SettingName::DefaultFastModel,
        fallback_selection,
    )
    .await
    {
        return resolve_available_selection(selection, available_agent_kinds);
    }

    load_default_smart_agent_selection_from_repositories(
        repositories,
        project_id,
        fallback_selection,
        available_agent_kinds,
    )
    .await
}

/// Loads the persisted smart-model default from repositories as an agent/model
/// selection.
///
/// This preserves explicit provider settings for shared model ids while
/// retaining model-only fallback behavior for older projects.
pub(crate) async fn load_default_smart_agent_selection_from_repositories(
    repositories: &AppRepositories,
    project_id: Option<i64>,
    fallback_selection: AgentSelection,
    available_agent_kinds: &[AgentKind],
) -> AgentSelection {
    let fallback_selection = resolve_available_selection(fallback_selection, available_agent_kinds);

    if let Some(selection) = load_model_selection_setting(
        repositories,
        project_id,
        SettingName::DefaultSmartAgent,
        SettingName::DefaultSmartModel,
        fallback_selection,
    )
    .await
    {
        return resolve_available_selection(selection, available_agent_kinds);
    }

    fallback_selection
}

/// Manages user-configurable application settings.
pub struct SettingsManager {
    /// Default agent/model selection used by fast-path workflows.
    pub default_fast_selection: AgentSelection,
    /// Default agent/model selection used by review workflows.
    pub default_review_selection: AgentSelection,
    /// Default agent/model selection used when creating new sessions.
    pub default_smart_selection: AgentSelection,
    /// Optional command run in tmux when opening a session worktree.
    pub launch_configuration: String,
    /// Default reasoning effort preference for models that support this
    /// setting.
    ///
    /// Currently applied to Codex and Claude turns.
    pub reasoning_level: ReasoningLevel,
    /// Active terminal color theme for the whole application.
    pub theme: ColorTheme,
    available_agent_kinds: Vec<AgentKind>,
    /// Whether generated session commit messages append the Agentty coauthor
    /// trailer for the active project.
    ///
    /// New projects start with this disabled until the user explicitly enables
    /// it.
    include_coauthored_by_agentty: bool,
    /// Active project identifier that owns these persisted settings.
    project_id: i64,
    repositories: AppRepositories,
    use_last_used_model_as_default: bool,
}

impl SettingsManager {
    /// Loads persisted settings using only the repositories and available
    /// provider capability required by this feature.
    pub async fn from_repositories(
        repositories: AppRepositories,
        available_agent_kinds: Vec<AgentKind>,
        project_id: i64,
    ) -> Self {
        let default_smart_fallback = fallback_selection_for_available_model(
            AgentKind::Antigravity.default_model(),
            &available_agent_kinds,
        );
        let default_smart_agent = load_default_smart_agent_selection_from_repositories(
            &repositories,
            Some(project_id),
            default_smart_fallback,
            &available_agent_kinds,
        )
        .await;

        let default_fast_agent = load_default_fast_agent_selection_from_repositories(
            &repositories,
            Some(project_id),
            default_smart_agent,
            &available_agent_kinds,
        )
        .await;

        let default_review_agent = load_model_selection_setting(
            &repositories,
            Some(project_id),
            SettingName::DefaultReviewAgent,
            SettingName::DefaultReviewModel,
            default_smart_agent,
        )
        .await
        .map_or(default_smart_agent, |selection| {
            resolve_available_selection(selection, &available_agent_kinds)
        });
        let reasoning_level = repositories
            .settings()
            .load_project_reasoning_level(project_id)
            .await
            .unwrap_or_default();

        let launch_configuration = repositories
            .settings()
            .get_project_setting(project_id, SettingName::LaunchConfiguration)
            .await
            .unwrap_or(None)
            .unwrap_or_default();

        let include_coauthored_by_agentty = load_project_bool_setting_from_repositories(
            &repositories,
            Some(project_id),
            SettingName::IncludeCoauthoredByAgentty,
            false,
        )
        .await;
        let use_last_used_model_as_default = load_project_bool_setting_from_repositories(
            &repositories,
            Some(project_id),
            SettingName::LastUsedModelAsDefault,
            false,
        )
        .await;
        let theme = load_theme_setting_from_repositories(&repositories).await;

        Self {
            default_fast_selection: default_fast_agent,
            default_review_selection: default_review_agent,
            default_smart_selection: default_smart_agent,
            launch_configuration,
            reasoning_level,
            theme,
            available_agent_kinds,
            include_coauthored_by_agentty,
            project_id,
            repositories,
            use_last_used_model_as_default,
        }
    }

    /// Returns configured launch configurations in persisted order.
    ///
    /// Commands are split by newlines and trimmed.
    #[must_use]
    pub fn launch_configurations(&self) -> Vec<String> {
        parse_launch_configurations(self.launch_configuration.as_str())
    }

    /// Returns an immutable projection for the settings screen.
    pub(crate) fn view(&self) -> SettingsView {
        SettingsView {
            available_model_selections: selectable_model_options(&self.available_agent_kinds)
                .into_iter()
                .map(ModelSelectorOption::selection)
                .collect(),
            default_fast_selection: self.default_fast_selection,
            default_review_selection: self.default_review_selection,
            default_smart_selection: self.default_smart_selection,
            include_coauthored_by_agentty: self.include_coauthored_by_agentty,
            launch_configuration: self.launch_configuration.clone(),
            reasoning_level: self.reasoning_level,
            theme: self.theme,
            use_last_used_model_as_default: self.use_last_used_model_as_default,
        }
    }

    /// Applies and persists one value change requested by the settings screen.
    pub(crate) async fn apply_operation(&mut self, operation: SettingsOperation) {
        match operation {
            SettingsOperation::DefaultFastSelection(selection) => {
                self.default_fast_selection = selection;
                self.persist_default_fast_model_setting().await;
            }
            SettingsOperation::DefaultReviewSelection(selection) => {
                self.default_review_selection = selection;
                self.persist_default_review_model_setting().await;
            }
            SettingsOperation::DefaultSmartSelection {
                selection,
                use_last_used_model_as_default,
            } => {
                self.default_smart_selection = selection;
                self.use_last_used_model_as_default = use_last_used_model_as_default;
                self.persist_default_smart_model_settings().await;
            }
            SettingsOperation::IncludeCoauthoredByAgentty(value) => {
                self.include_coauthored_by_agentty = value;
                self.persist_include_coauthored_by_agentty_setting().await;
            }
            SettingsOperation::LaunchConfiguration(value) => {
                self.launch_configuration = value;
                self.persist_launch_configuration_setting().await;
            }
            SettingsOperation::ReasoningLevel(value) => {
                self.reasoning_level = value;
                self.persist_reasoning_level_setting().await;
            }
            SettingsOperation::Theme(value) => {
                self.theme = value;
                self.persist_theme_setting().await;
            }
        }
    }

    /// Persists the current `LaunchConfiguration` setting value.
    async fn persist_launch_configuration_setting(&self) {
        let _ = self
            .repositories
            .settings()
            .upsert_project_setting(
                self.project_id,
                SettingName::LaunchConfiguration,
                &self.launch_configuration,
            )
            .await;
    }

    /// Atomically persists smart-model selector values (`DefaultSmartAgent`,
    /// `DefaultSmartModel`, and `LastUsedModelAsDefault`).
    async fn persist_default_smart_model_settings(&self) {
        let last_used_model_as_default_value = self.use_last_used_model_as_default.to_string();

        if let Err(error) = self
            .repositories
            .settings()
            .upsert_project_settings(
                self.project_id,
                vec![
                    (
                        SettingName::DefaultSmartModel,
                        self.default_smart_selection.model().as_str().to_string(),
                    ),
                    (
                        SettingName::DefaultSmartAgent,
                        self.default_smart_selection.kind().name().to_string(),
                    ),
                    (
                        SettingName::LastUsedModelAsDefault,
                        last_used_model_as_default_value,
                    ),
                ],
            )
            .await
        {
            warn!(
                project_id = self.project_id,
                error = %error,
                "failed to persist default smart model settings"
            );
        }
    }

    /// Persists the reasoning-level selector value (`ReasoningLevel`).
    async fn persist_reasoning_level_setting(&self) {
        // Best-effort: settings persistence failure is non-critical.
        let _ = self
            .repositories
            .settings()
            .set_project_reasoning_level(self.project_id, self.reasoning_level)
            .await;
    }

    /// Atomically persists the fast-model selector values (`DefaultFastAgent`
    /// and `DefaultFastModel`).
    async fn persist_default_fast_model_setting(&self) {
        if let Err(error) = self
            .repositories
            .settings()
            .upsert_project_settings(
                self.project_id,
                vec![
                    (
                        SettingName::DefaultFastModel,
                        self.default_fast_selection.model().as_str().to_string(),
                    ),
                    (
                        SettingName::DefaultFastAgent,
                        self.default_fast_selection.kind().name().to_string(),
                    ),
                ],
            )
            .await
        {
            warn!(
                project_id = self.project_id,
                error = %error,
                "failed to persist default fast model settings"
            );
        }
    }

    /// Atomically persists the review-model selector values
    /// (`DefaultReviewAgent` and `DefaultReviewModel`).
    async fn persist_default_review_model_setting(&self) {
        if let Err(error) = self
            .repositories
            .settings()
            .upsert_project_settings(
                self.project_id,
                vec![
                    (
                        SettingName::DefaultReviewModel,
                        self.default_review_selection.model().as_str().to_string(),
                    ),
                    (
                        SettingName::DefaultReviewAgent,
                        self.default_review_selection.kind().name().to_string(),
                    ),
                ],
            )
            .await
        {
            warn!(
                project_id = self.project_id,
                error = %error,
                "failed to persist default review model settings"
            );
        }
    }

    /// Persists the coauthor-trailer toggle for generated session commit
    /// messages.
    async fn persist_include_coauthored_by_agentty_setting(&self) {
        let include_coauthored_by_agentty = self.include_coauthored_by_agentty.to_string();

        // Best-effort: settings persistence failure is non-critical.
        let _ = self
            .repositories
            .settings()
            .upsert_project_setting(
                self.project_id,
                SettingName::IncludeCoauthoredByAgentty,
                &include_coauthored_by_agentty,
            )
            .await;
    }

    /// Persists the global terminal color theme selection.
    async fn persist_theme_setting(&self) {
        // Best-effort: settings persistence failure is non-critical.
        let _ = self
            .repositories
            .settings()
            .upsert_setting(SettingName::Theme, self.theme.as_str())
            .await;
    }
}

/// One provider-owned model option shown by settings selectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModelSelectorOption {
    agent_kind: AgentKind,
    model: AgentModel,
}

impl ModelSelectorOption {
    /// Returns this provider-owned option as a coherent agent/model
    /// selection.
    fn selection(self) -> AgentSelection {
        AgentSelection::new(self.agent_kind, self.model)
    }
}

/// Parses the persisted settings value into executable launch-configuration
/// entries.
fn parse_launch_configurations(launch_configuration_setting: &str) -> Vec<String> {
    launch_configuration_setting
        .lines()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(std::string::ToString::to_string)
        .collect()
}

/// Loads one project-scoped boolean setting through the narrow repository
/// dependency used by [`SettingsManager`].
async fn load_project_bool_setting_from_repositories(
    repositories: &AppRepositories,
    project_id: Option<i64>,
    setting_name: SettingName,
    default_value: bool,
) -> bool {
    let Some(project_id) = project_id else {
        return default_value;
    };

    repositories
        .settings()
        .get_project_setting(project_id, setting_name)
        .await
        .unwrap_or(None)
        .and_then(|setting_value| setting_value.parse::<bool>().ok())
        .unwrap_or(default_value)
}

/// Returns all selectable model options in settings display order for the
/// locally available providers.
fn selectable_model_options(available_agent_kinds: &[AgentKind]) -> Vec<ModelSelectorOption> {
    available_agent_kinds
        .iter()
        .copied()
        .flat_map(|agent_kind| {
            agent_kind
                .models()
                .iter()
                .copied()
                .map(move |model| ModelSelectorOption { agent_kind, model })
        })
        .collect()
}

/// Resolves one stored model against the currently available agent kinds.
fn resolve_available_model(
    model: AgentModel,
    available_agent_kinds: &[AgentKind],
    fallback_model: AgentModel,
) -> AgentModel {
    agent::resolve_model_for_available_agent_kinds(model, available_agent_kinds, fallback_model)
}

/// Resolves one stored agent/model selection against the currently available
/// agent kinds.
fn resolve_available_selection(
    selection: AgentSelection,
    available_agent_kinds: &[AgentKind],
) -> AgentSelection {
    if available_agent_kinds.contains(&selection.kind())
        && selection.kind().supports_model(selection.model())
    {
        return selection;
    }

    let model =
        resolve_available_model(selection.model(), available_agent_kinds, selection.model());
    let fallback_agent_kind = available_agent_kinds
        .first()
        .copied()
        .unwrap_or(selection.kind());
    let agent_kind =
        agent::resolve_agent_kind_for_model(model, available_agent_kinds, fallback_agent_kind);

    AgentSelection::new(agent_kind, model)
}

/// Loads a model setting and parses it into an [`AgentModel`].
///
/// Retired persisted model ids are upgraded to their current replacement
/// models before the value is returned.
async fn load_model_setting(
    repositories: &AppRepositories,
    project_id: Option<i64>,
    setting_name: SettingName,
) -> Option<AgentModel> {
    let project_id = project_id?;

    repositories
        .settings()
        .get_project_setting(project_id, setting_name)
        .await
        .unwrap_or(None)
        .and_then(|setting_value| AgentModel::parse_persisted(&setting_value).ok())
}

/// Loads one project-scoped model setting and its owning agent setting.
///
/// Older projects may only have a model key; in that case the fallback
/// selection and available-provider resolution decide ownership.
async fn load_model_selection_setting(
    repositories: &AppRepositories,
    project_id: Option<i64>,
    agent_setting_name: SettingName,
    model_setting_name: SettingName,
    fallback_selection: AgentSelection,
) -> Option<AgentSelection> {
    let model = load_model_setting(repositories, project_id, model_setting_name).await?;
    let agent_kind = load_agent_setting(repositories, project_id, agent_setting_name)
        .await
        .filter(|agent_kind| agent_kind.supports_model(model))
        .unwrap_or_else(|| fallback_agent_kind_for_model(model, fallback_selection.kind()));

    Some(AgentSelection::new(agent_kind, model))
}

/// Loads one project-scoped agent setting.
async fn load_agent_setting(
    repositories: &AppRepositories,
    project_id: Option<i64>,
    setting_name: SettingName,
) -> Option<AgentKind> {
    let project_id = project_id?;

    repositories
        .settings()
        .get_project_setting(project_id, setting_name)
        .await
        .unwrap_or(None)
        .and_then(|setting_value| setting_value.parse::<AgentKind>().ok())
}

/// Returns a compatible provider for model-only legacy settings.
fn fallback_agent_kind_for_model(model: AgentModel, fallback_agent_kind: AgentKind) -> AgentKind {
    if fallback_agent_kind.supports_model(model) {
        return fallback_agent_kind;
    }

    AgentKind::ALL
        .iter()
        .copied()
        .find(|agent_kind| agent_kind.supports_model(model))
        .unwrap_or(fallback_agent_kind)
}

/// Returns a coherent fallback selection for one model-only caller using
/// available provider order.
fn fallback_selection_for_available_model(
    model: AgentModel,
    available_agent_kinds: &[AgentKind],
) -> AgentSelection {
    let fallback_agent_kind = fallback_agent_kind_for_model(model, AgentKind::Antigravity);
    let agent_kind =
        agent::resolve_agent_kind_for_model(model, available_agent_kinds, fallback_agent_kind);

    AgentSelection::new(agent_kind, model)
}

/// Loads the persisted terminal color theme through the settings repository.
async fn load_theme_setting_from_repositories(repositories: &AppRepositories) -> ColorTheme {
    repositories
        .settings()
        .get_setting(SettingName::Theme)
        .await
        .unwrap_or(None)
        .and_then(|setting_value| ColorTheme::parse_persisted(&setting_value))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use ag_agent::MockAppServerClient;
    use ag_forge as forge;
    use ag_git as git;
    use tokio::sync::mpsc;

    use super::*;
    use crate::db::AppRepositories;
    use crate::domain::input::InputCommand;
    use crate::infra::fs;
    use crate::presentation::settings::{
        LaunchConfigurationListEditorMode, LaunchConfigurationListEditorSnapshot, SettingsAction,
        SettingsPresentationState, SettingsSelectorDropdown,
    };

    /// Builds app services backed by an in-memory database for settings tests.
    async fn test_services() -> (AppServices, i64) {
        let database = AppRepositories::in_memory().await;
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
                available_agent_kinds: AgentKind::ALL.to_vec(),
                clipboard_image_client_override: None,
                fs_client: Arc::new(fs::MockFsClient::new()),
                git_client: Arc::new(git::MockGitClient::new()),
                one_shot_client_override: None,
                personality_catalog_client_override: None,
                repositories: database.clone(),
                review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            },
            crate::domain::agent::AgentCliInfo::from_kinds(AgentKind::ALL),
        );

        (services, project_id)
    }

    /// Test-local settings screen harness that composes production value and
    /// presentation boundaries without extending `SettingsManager`.
    struct SettingsTestHarness {
        manager: Option<SettingsManager>,
        presentation: SettingsPresentationState,
        view: SettingsView,
    }

    impl SettingsTestHarness {
        fn new() -> Self {
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
                    default_fast_selection: default_selection,
                    default_review_selection: default_selection,
                    default_smart_selection: default_selection,
                    include_coauthored_by_agentty: false,
                    launch_configuration: String::new(),
                    reasoning_level: ReasoningLevel::High,
                    theme: ColorTheme::Current,
                    use_last_used_model_as_default: false,
                },
            }
        }

        fn from_manager(manager: SettingsManager) -> Self {
            Self {
                view: manager.view(),
                manager: Some(manager),
                presentation: SettingsPresentationState::default(),
            }
        }

        fn apply(&mut self, action: SettingsAction) -> Option<SettingsOperation> {
            self.presentation.apply(&self.view, action)
        }

        async fn apply_and_persist(&mut self, action: SettingsAction) {
            let Some(operation) = self.apply(action) else {
                return;
            };

            self.persist_operation(operation).await;
        }

        async fn persist_operation(&mut self, operation: SettingsOperation) {
            let manager = self
                .manager
                .as_mut()
                .expect("persisted settings test requires a manager");
            manager.apply_operation(operation).await;
            self.view = manager.view();
        }

        fn fixture_view_mut(&mut self) -> &mut SettingsView {
            assert!(
                self.manager.is_none(),
                "persisted settings fixtures must be seeded through repositories"
            );

            &mut self.view
        }

        fn settings(&self) -> &SettingsManager {
            self.manager
                .as_ref()
                .expect("test requires repository-backed settings")
        }

        fn next(&mut self) {
            let _ = self.apply(SettingsAction::Next);
        }

        fn previous(&mut self) {
            let _ = self.apply(SettingsAction::Previous);
        }

        fn handle_enter(&mut self) {
            let _ = self.apply(SettingsAction::Activate);
        }

        fn is_launch_configuration_list_editor_open(&self) -> bool {
            self.presentation.is_launch_configuration_list_editor_open()
        }

        fn is_selector_dropdown_open(&self) -> bool {
            self.presentation.is_selector_dropdown_open()
        }

        fn launch_configuration_list_editor(
            &self,
        ) -> Option<LaunchConfigurationListEditorSnapshot> {
            self.presentation
                .snapshot(&self.view)
                .launch_configuration_list_editor
        }

        fn launch_configurations(&self) -> Vec<String> {
            parse_launch_configurations(self.view.launch_configuration.as_str())
        }

        fn selector_dropdown(&self) -> Option<SettingsSelectorDropdown> {
            self.presentation.snapshot(&self.view).selector_dropdown
        }

        fn start_adding_launch_configuration(&mut self) {
            let _ = self.apply(SettingsAction::StartAddingLaunchConfiguration);
        }

        fn start_editing_selected_launch_configuration(&mut self) {
            let _ = self.apply(SettingsAction::EditLaunchConfiguration);
        }

        fn cancel_launch_configuration_input(&mut self) {
            let _ = self.apply(SettingsAction::Cancel);
        }

        fn apply_launch_configuration_input_command(&mut self, command: InputCommand) {
            let _ = self.apply(SettingsAction::Input(command));
        }

        fn next_launch_configuration_list_editor_item(&mut self) {
            self.next();
        }

        fn next_selector_dropdown_option(&mut self) {
            self.next();
        }

        async fn confirm_launch_configuration_input(&mut self) {
            self.apply_and_persist(SettingsAction::Confirm).await;
        }

        async fn delete_selected_launch_configuration(&mut self) {
            self.apply_and_persist(SettingsAction::DeleteLaunchConfiguration)
                .await;
        }

        async fn move_selected_launch_configuration_down(&mut self) {
            self.apply_and_persist(SettingsAction::MoveLaunchConfigurationDown)
                .await;
        }

        async fn move_selected_launch_configuration_up(&mut self) {
            self.apply_and_persist(SettingsAction::MoveLaunchConfigurationUp)
                .await;
        }

        async fn select_selector_dropdown_option(&mut self) {
            self.apply_and_persist(SettingsAction::Confirm).await;
        }

        fn settings_rows(&self) -> Vec<(&'static str, String)> {
            let snapshot = self.presentation.snapshot(&self.view);

            snapshot
                .global_rows
                .into_iter()
                .chain(snapshot.project_rows)
                .collect()
        }

        fn global_settings_rows(&self) -> Vec<(&'static str, String)> {
            self.presentation.snapshot(&self.view).global_rows
        }

        fn project_settings_rows(&self) -> Vec<(&'static str, String)> {
            self.presentation.snapshot(&self.view).project_rows
        }

        fn footer_hint(&self) -> &'static str {
            self.presentation.snapshot(&self.view).footer_hint
        }
    }

    /// Selects one settings row through the screen navigation action.
    fn select_row(manager: &mut SettingsTestHarness, row_index: usize) {
        for _ in 0..row_index {
            manager.next();
        }
    }

    /// Creates an in-memory settings-screen fixture.
    fn new_settings_manager() -> SettingsTestHarness {
        SettingsTestHarness::new()
    }

    /// Loads a settings screen through the production repository boundary.
    async fn settings_manager(services: &AppServices, project_id: i64) -> SettingsTestHarness {
        let manager = SettingsManager::from_repositories(
            services.db().clone(),
            services.available_agent_kinds(),
            project_id,
        )
        .await;

        SettingsTestHarness::from_manager(manager)
    }

    /// Persists a launch-configuration fixture before loading the production
    /// settings boundary.
    async fn settings_manager_with_launch_configuration(
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

    #[test]
    fn setting_name_as_str_returns_default_fast_model() {
        // Arrange

        // Act
        let setting_name = SettingName::DefaultFastModel.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultFastModel");
    }

    #[test]
    fn setting_name_as_str_returns_default_fast_agent() {
        // Arrange

        // Act
        let setting_name = SettingName::DefaultFastAgent.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultFastAgent");
    }

    #[test]
    fn setting_name_as_str_returns_reasoning_level() {
        // Arrange

        // Act
        let setting_name = SettingName::ReasoningLevel.as_str();

        // Assert
        assert_eq!(setting_name, "ReasoningLevel");
    }

    #[test]
    fn setting_name_as_str_returns_default_smart_model() {
        // Arrange

        // Act
        let setting_name = SettingName::DefaultSmartModel.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultSmartModel");
    }

    #[test]
    fn setting_name_as_str_returns_default_smart_agent() {
        // Arrange

        // Act
        let setting_name = SettingName::DefaultSmartAgent.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultSmartAgent");
    }

    #[test]
    fn setting_name_as_str_returns_default_review_model() {
        // Arrange

        // Act
        let setting_name = SettingName::DefaultReviewModel.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultReviewModel");
    }

    #[test]
    fn setting_name_as_str_returns_default_review_agent() {
        // Arrange

        // Act
        let setting_name = SettingName::DefaultReviewAgent.as_str();

        // Assert
        assert_eq!(setting_name, "DefaultReviewAgent");
    }

    #[test]
    fn setting_name_as_str_returns_include_coauthored_by_agentty() {
        // Arrange

        // Act
        let setting_name = SettingName::IncludeCoauthoredByAgentty.as_str();

        // Assert
        assert_eq!(setting_name, "IncludeCoauthoredByAgentty");
    }

    #[test]
    fn setting_name_as_str_returns_launch_configuration() {
        // Arrange

        // Act
        let setting_name = SettingName::LaunchConfiguration.as_str();

        // Assert
        assert_eq!(setting_name, "LaunchConfiguration");
    }

    #[test]
    fn setting_name_as_str_returns_last_used_model_as_default() {
        // Arrange

        // Act
        let setting_name = SettingName::LastUsedModelAsDefault.as_str();

        // Assert
        assert_eq!(setting_name, "LastUsedModelAsDefault");
    }

    #[test]
    fn setting_name_as_str_returns_theme() {
        // Arrange

        // Act
        let setting_name = SettingName::Theme.as_str();

        // Assert
        assert_eq!(setting_name, "Theme");
    }

    #[tokio::test]
    async fn load_default_smart_model_setting_prefers_project_override() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                AgentModel::Gpt55.as_str(),
            )
            .await
            .expect("failed to persist smart model");

        // Act
        let loaded_model = load_default_smart_model_setting(
            &services,
            Some(project_id),
            AgentModel::ClaudeHaiku4520251001,
        )
        .await;

        // Assert
        assert_eq!(loaded_model, AgentModel::Gpt55);
    }

    #[tokio::test]
    async fn load_default_smart_agent_setting_prefers_persisted_agent() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .settings()
            .upsert_project_setting(project_id, SettingName::DefaultSmartAgent, "gemini")
            .await
            .expect("failed to persist smart agent");
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                AgentModel::Gemini31ProPreview.as_str(),
            )
            .await
            .expect("failed to persist smart model");

        // Act
        let loaded_selection = load_default_smart_agent_setting(
            &services,
            Some(project_id),
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini36Flash),
        )
        .await;

        // Assert
        assert_eq!(
            loaded_selection,
            AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31ProPreview)
        );
    }

    #[tokio::test]
    async fn load_default_smart_model_setting_falls_back_to_default() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                "not-a-valid-model",
            )
            .await
            .expect("failed to persist invalid smart model");

        // Act
        let fallback_loaded_model = load_default_smart_model_setting(
            &services,
            Some(project_id),
            AgentModel::ClaudeHaiku4520251001,
        )
        .await;

        // Assert
        assert_eq!(fallback_loaded_model, AgentModel::ClaudeHaiku4520251001);
    }

    #[tokio::test]
    async fn load_default_fast_agent_setting_migrates_retired_claude_opus_46_setting() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                "claude-opus-4-6",
            )
            .await
            .expect("failed to persist smart model");

        // Act
        let fallback_fast_selection = load_default_fast_agent_setting(
            &services,
            Some(project_id),
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt53CodexSpark),
        )
        .await;

        // Assert
        assert_eq!(
            fallback_fast_selection,
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus48)
        );

        // Arrange
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultFastModel,
                AgentModel::Gpt55.as_str(),
            )
            .await
            .expect("failed to persist fast model");

        // Act
        let explicit_fast_selection = load_default_fast_agent_setting(
            &services,
            Some(project_id),
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt53CodexSpark),
        )
        .await;

        // Assert
        assert_eq!(
            explicit_fast_selection,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt55)
        );
    }

    #[tokio::test]
    async fn settings_manager_new_loads_project_scoped_values() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                AgentModel::Gpt55.as_str(),
            )
            .await
            .expect("failed to persist project smart model");
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultFastModel,
                AgentModel::Gpt53CodexSpark.as_str(),
            )
            .await
            .expect("failed to persist project fast model");
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultReviewModel,
                "claude-opus-4-6",
            )
            .await
            .expect("failed to persist review model");
        services
            .db()
            .settings()
            .upsert_project_setting(project_id, SettingName::IncludeCoauthoredByAgentty, "false")
            .await
            .expect("failed to persist coauthor setting");
        services
            .db()
            .settings()
            .upsert_project_setting(project_id, SettingName::LaunchConfiguration, "nvim .")
            .await
            .expect("failed to persist launch configuration");
        services
            .db()
            .settings()
            .set_project_reasoning_level(project_id, ReasoningLevel::Low)
            .await
            .expect("failed to persist reasoning level");
        services
            .db()
            .settings()
            .upsert_project_setting(project_id, SettingName::LastUsedModelAsDefault, "true")
            .await
            .expect("failed to persist last-used-model flag");
        services
            .db()
            .settings()
            .upsert_setting(SettingName::Theme, ColorTheme::Green.as_str())
            .await
            .expect("failed to persist theme setting");

        // Act
        let manager = settings_manager(&services, project_id).await;
        let settings = manager.settings();

        // Assert
        assert_eq!(
            settings.default_smart_selection,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt55)
        );
        assert_eq!(
            settings.default_fast_selection,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt53CodexSpark)
        );
        assert_eq!(
            settings.default_review_selection,
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus48)
        );
        assert_eq!(settings.launch_configuration, "nvim .");
        assert_eq!(settings.reasoning_level, ReasoningLevel::Low);
        assert_eq!(settings.theme, ColorTheme::Green);
        assert!(!settings.include_coauthored_by_agentty);
        assert!(settings.use_last_used_model_as_default);
    }

    #[tokio::test]
    async fn settings_manager_new_defaults_invalid_last_used_model_flag_to_false() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::LastUsedModelAsDefault,
                "invalid-bool",
            )
            .await
            .expect("failed to persist invalid flag");

        // Act
        let manager = settings_manager(&services, project_id).await;

        // Assert
        assert!(!manager.settings().use_last_used_model_as_default);
    }

    #[tokio::test]
    async fn settings_manager_new_defaults_invalid_coauthor_flag_to_false() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::IncludeCoauthoredByAgentty,
                "invalid-bool",
            )
            .await
            .expect("failed to persist invalid coauthor flag");

        // Act
        let manager = settings_manager(&services, project_id).await;

        // Assert
        assert!(!manager.settings().include_coauthored_by_agentty);
    }

    #[tokio::test]
    async fn settings_manager_new_defaults_invalid_theme_to_current() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .settings()
            .upsert_setting(SettingName::Theme, "invalid-theme")
            .await
            .expect("failed to persist invalid theme");

        // Act
        let manager = settings_manager(&services, project_id).await;

        // Assert
        assert_eq!(manager.settings().theme, ColorTheme::Current);
    }

    #[tokio::test]
    async fn settings_manager_new_loads_persisted_dark_horizon_theme() {
        // Arrange
        let (services, project_id) = test_services().await;
        services
            .db()
            .settings()
            .upsert_setting(SettingName::Theme, ColorTheme::DarkHorizon.as_str())
            .await
            .expect("failed to persist theme setting");

        // Act
        let manager = settings_manager(&services, project_id).await;

        // Assert
        assert_eq!(manager.settings().theme, ColorTheme::DarkHorizon);
    }

    #[tokio::test]
    async fn apply_operation_persists_fast_review_and_reasoning_settings() {
        // Arrange
        let (services, project_id) = test_services().await;
        let mut manager = settings_manager(&services, project_id).await;
        let fast_selection = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt55);
        let review_selection = AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus48);

        // Act
        manager
            .persist_operation(SettingsOperation::DefaultFastSelection(fast_selection))
            .await;
        manager
            .persist_operation(SettingsOperation::DefaultReviewSelection(review_selection))
            .await;
        manager
            .persist_operation(SettingsOperation::ReasoningLevel(ReasoningLevel::Max))
            .await;

        // Assert
        assert_eq!(manager.settings().default_fast_selection, fast_selection);
        assert_eq!(
            manager.settings().default_review_selection,
            review_selection
        );
        assert_eq!(manager.settings().reasoning_level, ReasoningLevel::Max);
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, SettingName::DefaultFastAgent)
                .await
                .expect("failed to load fast agent"),
            Some(AgentKind::Codex.name().to_string())
        );
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, SettingName::DefaultFastModel)
                .await
                .expect("failed to load fast model"),
            Some(AgentModel::Gpt55.as_str().to_string())
        );
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, SettingName::DefaultReviewAgent)
                .await
                .expect("failed to load review agent"),
            Some(AgentKind::Claude.name().to_string())
        );
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, SettingName::DefaultReviewModel)
                .await
                .expect("failed to load review model"),
            Some(AgentModel::ClaudeOpus48.as_str().to_string())
        );
        assert_eq!(
            services
                .db()
                .settings()
                .load_project_reasoning_level(project_id)
                .await
                .expect("failed to load reasoning level"),
            ReasoningLevel::Max
        );
    }

    #[test]
    fn next_moves_selection_to_default_reasoning_level_row() {
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
    fn previous_wraps_to_launch_configurations_row_from_theme_row() {
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
            Some(6)
        );
    }

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
    fn settings_rows_include_reasoning_model_coauthor_and_launch_configuration_options() {
        // Arrange
        let manager = new_settings_manager();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[0].0, "Theme");
        assert_eq!(rows[1].0, "Default Reasoning Level");
        assert_eq!(rows[2].0, "Default Smart Model");
        assert_eq!(rows[3].0, "Default Fast Model");
        assert_eq!(rows[4].0, "Default Review Model");
        assert_eq!(rows[5].0, "Coauthored by Agentty");
        assert_eq!(rows[6].0, "Launch Configurations");
    }

    #[test]
    fn settings_rows_split_theme_into_global_and_project_sections() {
        // Arrange
        let manager = new_settings_manager();

        // Act
        let global_rows = manager.global_settings_rows();
        let project_rows = manager.project_settings_rows();

        // Assert
        assert_eq!(global_rows.len(), 1);
        assert_eq!(global_rows[0].0, "Theme");
        assert_eq!(project_rows.len(), 6);
        assert_eq!(project_rows[0].0, "Default Reasoning Level");
        assert_eq!(project_rows[1].0, "Default Smart Model");
        assert_eq!(project_rows[2].0, "Default Fast Model");
        assert_eq!(project_rows[3].0, "Default Review Model");
        assert_eq!(project_rows[4].0, "Coauthored by Agentty");
        assert_eq!(project_rows[5].0, "Launch Configurations");
    }

    #[test]
    fn footer_hint_returns_launch_configuration_input_hint_when_input_is_active() {
        // Arrange
        let mut manager = new_settings_manager();
        select_row(&mut manager, 6);
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
        manager.fixture_view_mut().launch_configuration =
            " cargo test \n npm run dev \n".to_string();

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

    #[test]
    fn settings_rows_show_empty_placeholder_for_launch_configuration() {
        // Arrange
        let manager = new_settings_manager();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[6].1, "(none)");
    }

    #[test]
    fn settings_rows_show_single_launch_configuration_summary() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().launch_configuration = "http://localhost:5173".to_string();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[6].1, "http://localhost:5173");
    }

    #[test]
    fn settings_rows_show_multiple_launch_configuration_summary() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().launch_configuration =
            "cargo test\nnpm run dev\nlazygit".to_string();

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[6].1, "cargo test (+2 more)");
    }

    #[test]
    fn settings_rows_show_last_used_model_as_default_value_when_enabled() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().use_last_used_model_as_default = true;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[2].1, "Last used model as default");
    }

    #[test]
    fn settings_rows_show_default_smart_model_with_agent_prefix() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().default_smart_selection =
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini31ProPreview);

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[2].1, "antigravity/gemini-3.1-pro-preview");
    }

    #[test]
    fn settings_rows_show_default_smart_model_with_real_gemini_agent() {
        // Arrange
        let mut manager = new_settings_manager();
        let view = manager.fixture_view_mut();
        view.available_model_selections = selectable_model_options(&[AgentKind::Gemini])
            .into_iter()
            .map(ModelSelectorOption::selection)
            .collect();
        view.default_smart_selection =
            AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31ProPreview);

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[2].1, "gemini/gemini-3.1-pro-preview");
    }

    #[test]
    fn settings_rows_show_default_fast_model_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().default_fast_selection =
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt55);

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[3].1, "codex/gpt-5.5");
    }

    #[test]
    fn settings_rows_show_default_review_model_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().default_review_selection =
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus48);

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[4].1, "claude/claude-opus-4-8");
    }

    #[test]
    fn settings_rows_show_coauthored_by_agentty_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().include_coauthored_by_agentty = false;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[5].1, "Disabled");
    }

    #[test]
    fn settings_rows_show_reasoning_level_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().reasoning_level = ReasoningLevel::Max;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[1].1, "max");
    }

    #[test]
    fn settings_rows_show_theme_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().theme = ColorTheme::Green;

        // Act
        let rows = manager.settings_rows();

        // Assert
        assert_eq!(rows[0].1, "Agentty Green");
    }

    #[test]
    fn handle_enter_opens_launch_configuration_list_editor() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().launch_configuration = "nvim .".to_string();
        select_row(&mut manager, 6);

        // Act
        manager.handle_enter();

        // Assert
        let editor = manager
            .launch_configuration_list_editor()
            .expect("expected launch-configuration list editor");
        assert_eq!(editor.commands, vec!["nvim .".to_string()]);
        assert_eq!(editor.mode, LaunchConfigurationListEditorMode::Browse);
    }

    #[test]
    fn next_and_previous_do_not_move_selection_while_launch_configuration_editor_is_open() {
        // Arrange
        let mut manager = new_settings_manager();
        select_row(&mut manager, 6);
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
            Some(6)
        );
        assert!(manager.is_launch_configuration_list_editor_open());
    }

    #[test]
    fn navigation_actions_do_not_request_launch_configuration_persistence() {
        // Arrange
        let mut manager = new_settings_manager();
        select_row(&mut manager, 6);
        manager.handle_enter();

        // Act
        let next_operation = manager.apply(SettingsAction::Next);
        let previous_operation = manager.apply(SettingsAction::Previous);

        // Assert
        assert_eq!(next_operation, None);
        assert_eq!(previous_operation, None);
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
    fn cancel_launch_configuration_input_returns_to_browse_without_changing_value() {
        // Arrange
        let mut manager = new_settings_manager();
        manager.fixture_view_mut().launch_configuration = "old command".to_string();
        select_row(&mut manager, 6);
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

    #[tokio::test]
    async fn confirm_launch_configuration_input_adds_trimmed_command_and_persists_value() {
        // Arrange
        let (services, project_id) = test_services().await;
        let mut manager = settings_manager(&services, project_id).await;
        select_row(&mut manager, 6);
        manager.handle_enter();
        manager.start_adding_launch_configuration();

        // Act
        manager.apply_launch_configuration_input_command(InputCommand::InsertText(
            " nvim ".to_string(),
        ));
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
        select_row(&mut manager, 6);
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
        select_row(&mut manager, 6);
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
        select_row(&mut manager, 6);
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
        select_row(&mut manager, 6);
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
    async fn selector_dropdown_selects_coauthor_setting_and_persists_value() {
        // Arrange
        let (services, project_id) = test_services().await;
        let mut manager = settings_manager(&services, project_id).await;
        select_row(&mut manager, 5);

        // Act
        manager.handle_enter();
        manager.next_selector_dropdown_option();
        manager.select_selector_dropdown_option().await;

        // Assert
        assert!(manager.settings().include_coauthored_by_agentty);
        assert!(!manager.is_selector_dropdown_open());
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, SettingName::IncludeCoauthoredByAgentty)
                .await
                .expect("failed to load coauthor setting"),
            Some("true".to_string())
        );
    }

    #[tokio::test]
    async fn selector_dropdown_selects_theme_setting_and_persists_value() {
        // Arrange
        let (services, project_id) = test_services().await;
        let mut manager = settings_manager(&services, project_id).await;
        select_row(&mut manager, 0);

        // Act
        manager.handle_enter();
        let dropdown = manager
            .selector_dropdown()
            .expect("expected theme selector dropdown");
        assert_eq!(dropdown.row_index, 0);
        assert_eq!(dropdown.selected_index, 0);
        assert_eq!(dropdown.options[1].label, "Agentty Green");

        manager.next_selector_dropdown_option();
        manager.select_selector_dropdown_option().await;
        let selected_theme = manager.settings().theme;
        let persisted_theme = services
            .db()
            .settings()
            .get_setting(SettingName::Theme)
            .await
            .expect("failed to load theme setting");

        // Assert
        assert_eq!(selected_theme, ColorTheme::Green);
        assert_eq!(
            persisted_theme,
            Some(ColorTheme::Green.as_str().to_string())
        );
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
        assert!(manager.settings().launch_configuration.is_empty());
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

    #[tokio::test]
    async fn selector_dropdown_persists_last_used_flag_and_explicit_smart_model() {
        // Arrange
        let (services, project_id) = test_services().await;
        let options = selectable_model_options(AgentKind::ALL);
        let last_option = *options.last().expect("model options should not be empty");
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartAgent,
                last_option.agent_kind.name(),
            )
            .await
            .expect("failed to persist smart agent fixture");
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                last_option.model.as_str(),
            )
            .await
            .expect("failed to persist smart model fixture");
        services
            .db()
            .settings()
            .upsert_project_setting(project_id, SettingName::LastUsedModelAsDefault, "false")
            .await
            .expect("failed to persist last-used fixture");
        let mut manager = settings_manager(&services, project_id).await;
        select_row(&mut manager, 2);

        // Act
        manager.handle_enter();
        let dropdown = manager
            .selector_dropdown()
            .expect("expected smart model selector dropdown");
        assert_eq!(dropdown.selected_index, options.len() - 1);
        manager.next_selector_dropdown_option();
        manager.select_selector_dropdown_option().await;

        // Assert
        assert!(manager.settings().use_last_used_model_as_default);
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, SettingName::LastUsedModelAsDefault)
                .await
                .expect("failed to load last-used flag"),
            Some("true".to_string())
        );

        // Act
        manager.handle_enter();
        manager.next_selector_dropdown_option();
        manager.select_selector_dropdown_option().await;

        // Assert
        assert!(!manager.settings().use_last_used_model_as_default);
        assert_eq!(
            manager.settings().default_smart_selection,
            options[0].selection()
        );
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, SettingName::DefaultSmartModel)
                .await
                .expect("failed to load smart model"),
            Some(options[0].model.as_str().to_string())
        );
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, SettingName::DefaultSmartAgent)
                .await
                .expect("failed to load smart agent"),
            Some(options[0].agent_kind.name().to_string())
        );
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, SettingName::LastUsedModelAsDefault)
                .await
                .expect("failed to load last-used flag"),
            Some("false".to_string())
        );
    }

    #[tokio::test]
    async fn load_default_smart_model_setting_falls_back_to_available_backend() {
        // Arrange
        let (mut services, project_id) = test_services().await;
        let available_agent_kinds = vec![AgentKind::Codex];
        services = AppServices::new_with_agent_clis(
            services.base_path().to_path_buf(),
            services.clock(),
            services.event_sender(),
            crate::app::service::AppServiceDeps {
                app_server_client_override: services.app_server_client_override(),
                available_agent_kinds: available_agent_kinds.clone(),
                clipboard_image_client_override: None,
                fs_client: services.fs_client(),
                git_client: services.git_client(),
                one_shot_client_override: Some(services.one_shot_client()),
                personality_catalog_client_override: Some(services.personality_catalog_client()),
                repositories: services.db().clone(),
                review_request_client: services.review_request_client(),
            },
            crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds),
        );
        services
            .db()
            .settings()
            .upsert_project_setting(
                project_id,
                SettingName::DefaultSmartModel,
                AgentModel::Gemini31ProPreview.as_str(),
            )
            .await
            .expect("failed to persist unavailable smart model");

        // Act
        let loaded_model = load_default_smart_model_setting(
            &services,
            Some(project_id),
            AgentKind::Antigravity.default_model(),
        )
        .await;

        // Assert
        assert_eq!(loaded_model, AgentKind::Codex.default_model());
    }
}
