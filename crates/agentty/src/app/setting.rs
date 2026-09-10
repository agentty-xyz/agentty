use tracing::warn;

use crate::app::AppServices;
use crate::domain::agent::{
    self, AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata, ReasoningLevel,
    ResponseStyle, SpeedMode,
};
use crate::domain::setting::{
    DEFAULT_AUTO_APPROVE_ORCHESTRATION_RESEARCH, DEFAULT_ORCHESTRATION_PARALLELISM,
    MAX_ORCHESTRATION_PARALLELISM, SettingName,
};
use crate::domain::theme::ColorTheme;
use crate::infra::db::AppRepositories;
use crate::presentation::setting::{SettingsOperation, SettingsView};

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

/// Loads the persisted review-agent defaults for one project.
///
/// Background review workflows use this when the owning project is not the
/// active UI project, so generation keeps the model, reasoning, and speed
/// configured for the session's project. Unconfigured projects use the same
/// provider-aware baseline as normal project loading, independent of the
/// active project's defaults.
pub(crate) async fn load_default_review_agent_setting(
    services: &AppServices,
    project_id: i64,
) -> crate::app::review::ReviewAgent {
    let available_agent_kinds = services.available_agent_kinds();
    let fallback_selection = default_smart_fallback_selection(&available_agent_kinds);
    let default_smart_agent = load_default_smart_agent_selection_from_repositories(
        services.db(),
        Some(project_id),
        fallback_selection,
        &available_agent_kinds,
    )
    .await;
    let default_review_agent = load_model_selection_setting(
        services.db(),
        Some(project_id),
        SettingName::DefaultReviewAgent,
        SettingName::DefaultReviewModel,
        default_smart_agent,
        &available_agent_kinds,
    )
    .await
    .unwrap_or(default_smart_agent);
    let defaults = load_model_role_defaults(
        services.db(),
        project_id,
        SettingName::DefaultReviewReasoningLevel,
        SettingName::DefaultReviewSpeedMode,
        default_review_agent,
    )
    .await;

    (
        defaults.selection,
        defaults.reasoning_level,
        defaults.speed_mode,
    )
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
        available_agent_kinds,
    )
    .await
    {
        return selection;
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
        available_agent_kinds,
    )
    .await
    {
        return selection;
    }

    fallback_selection
}

/// Loads one project-scoped role response-speed default.
pub(crate) async fn load_project_speed_mode_setting(
    repositories: &AppRepositories,
    project_id: Option<i64>,
    setting_name: SettingName,
) -> SpeedMode {
    let Some(project_id) = project_id else {
        return SpeedMode::Normal;
    };

    repositories
        .settings()
        .load_project_speed_mode(project_id, setting_name)
        .await
        .unwrap_or_default()
}

/// Loads and normalizes one project-scoped role speed default.
async fn load_normalized_speed_default(
    repositories: &AppRepositories,
    project_id: i64,
    setting_name: SettingName,
    selection: AgentSelection,
) -> (AgentSelection, SpeedMode) {
    let speed_mode =
        load_project_speed_mode_setting(repositories, Some(project_id), setting_name).await;

    normalized_speed_default(selection, speed_mode)
}

/// Model, reasoning, and speed defaults for one settings role.
struct ModelRoleDefaults {
    reasoning_level: ReasoningLevel,
    selection: AgentSelection,
    speed_mode: SpeedMode,
}

async fn load_model_role_defaults(
    repositories: &AppRepositories,
    project_id: i64,
    reasoning_setting_name: SettingName,
    speed_setting_name: SettingName,
    selection: AgentSelection,
) -> ModelRoleDefaults {
    let reasoning_level = repositories
        .settings()
        .load_project_reasoning_level(project_id, reasoning_setting_name)
        .await
        .unwrap_or_default();
    let (selection, speed_mode) =
        load_normalized_speed_default(repositories, project_id, speed_setting_name, selection)
            .await;

    ModelRoleDefaults {
        reasoning_level,
        selection,
        speed_mode,
    }
}

/// Manages user-configurable application settings.
pub struct SettingsManager {
    /// Whether temporary research-only orchestration waves start immediately.
    pub auto_approve_orchestration_research: bool,
    /// Default reasoning level used by fast-path workflows.
    pub default_fast_reasoning_level: ReasoningLevel,
    /// Default agent/model selection used by fast-path workflows.
    pub default_fast_selection: AgentSelection,
    /// Default response speed used by fast-path workflows.
    pub default_fast_speed_mode: SpeedMode,
    /// Default response style used when creating new sessions.
    pub default_response_style: ResponseStyle,
    /// Default reasoning level used by review workflows.
    pub default_review_reasoning_level: ReasoningLevel,
    /// Default agent/model selection used by review workflows.
    pub default_review_selection: AgentSelection,
    /// Default response speed used by review workflows.
    pub default_review_speed_mode: SpeedMode,
    /// Default reasoning level used when creating new sessions.
    pub default_smart_reasoning_level: ReasoningLevel,
    /// Default agent/model selection used when creating new sessions.
    pub default_smart_selection: AgentSelection,
    /// Default response speed used when creating new sessions.
    pub default_smart_speed_mode: SpeedMode,
    /// Optional command run in tmux when opening a session worktree.
    pub launch_configuration: String,
    /// Maximum number of orchestration child sessions run concurrently.
    pub orchestration_parallelism: u8,
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
        let default_smart_fallback = default_smart_fallback_selection(&available_agent_kinds);
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
            &available_agent_kinds,
        )
        .await
        .unwrap_or(default_smart_agent);
        let default_smart = load_model_role_defaults(
            &repositories,
            project_id,
            SettingName::DefaultSmartReasoningLevel,
            SettingName::DefaultSmartSpeedMode,
            default_smart_agent,
        )
        .await;
        let default_fast = load_model_role_defaults(
            &repositories,
            project_id,
            SettingName::DefaultFastReasoningLevel,
            SettingName::DefaultFastSpeedMode,
            default_fast_agent,
        )
        .await;
        let default_review = load_model_role_defaults(
            &repositories,
            project_id,
            SettingName::DefaultReviewReasoningLevel,
            SettingName::DefaultReviewSpeedMode,
            default_review_agent,
        )
        .await;
        let default_response_style =
            load_default_response_style_setting_from_repositories(&repositories, project_id).await;

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
        let orchestration_parallelism =
            load_orchestration_parallelism_setting_from_repositories(&repositories).await;
        let auto_approve_orchestration_research =
            load_auto_approve_orchestration_research_setting_from_repositories(&repositories).await;

        Self {
            auto_approve_orchestration_research,
            default_fast_reasoning_level: default_fast.reasoning_level,
            default_fast_selection: default_fast.selection,
            default_fast_speed_mode: default_fast.speed_mode,
            default_review_reasoning_level: default_review.reasoning_level,
            default_review_selection: default_review.selection,
            default_review_speed_mode: default_review.speed_mode,
            default_response_style,
            default_smart_reasoning_level: default_smart.reasoning_level,
            default_smart_selection: default_smart.selection,
            default_smart_speed_mode: default_smart.speed_mode,
            launch_configuration,
            theme,
            available_agent_kinds,
            include_coauthored_by_agentty,
            orchestration_parallelism,
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
            auto_approve_orchestration_research: self.auto_approve_orchestration_research,
            default_fast_reasoning_level: self.default_fast_reasoning_level,
            default_fast_selection: self.default_fast_selection,
            default_fast_speed_mode: self.default_fast_speed_mode,
            default_review_reasoning_level: self.default_review_reasoning_level,
            default_review_selection: self.default_review_selection,
            default_review_speed_mode: self.default_review_speed_mode,
            default_response_style: self.default_response_style,
            default_smart_reasoning_level: self.default_smart_reasoning_level,
            default_smart_selection: self.default_smart_selection,
            default_smart_speed_mode: self.default_smart_speed_mode,
            include_coauthored_by_agentty: self.include_coauthored_by_agentty,
            launch_configuration: self.launch_configuration.clone(),
            orchestration_parallelism: self.orchestration_parallelism,
            theme: self.theme,
            use_last_used_model_as_default: self.use_last_used_model_as_default,
        }
    }

    /// Applies and persists one value change requested by the settings screen.
    pub(crate) async fn apply_operation(&mut self, operation: SettingsOperation) {
        match operation {
            SettingsOperation::AutoApproveOrchestrationResearch(value) => {
                self.auto_approve_orchestration_research = value;
                self.persist_auto_approve_orchestration_research_setting()
                    .await;
            }
            SettingsOperation::DefaultFastSelection {
                reasoning_level,
                selection,
                speed_mode,
            } => {
                let (selection, speed_mode) = normalized_speed_default(selection, speed_mode);
                self.default_fast_reasoning_level = reasoning_level;
                self.default_fast_selection = selection;
                self.default_fast_speed_mode = speed_mode;
                self.persist_default_fast_model_setting().await;
            }
            SettingsOperation::DefaultReviewSelection {
                reasoning_level,
                selection,
                speed_mode,
            } => {
                let (selection, speed_mode) = normalized_speed_default(selection, speed_mode);
                self.default_review_reasoning_level = reasoning_level;
                self.default_review_selection = selection;
                self.default_review_speed_mode = speed_mode;
                self.persist_default_review_model_setting().await;
            }
            SettingsOperation::DefaultResponseStyle(value) => {
                self.default_response_style = value;
                self.persist_default_response_style_setting().await;
            }
            SettingsOperation::DefaultSmartSelection {
                reasoning_level,
                selection,
                speed_mode,
                use_last_used_model_as_default,
            } => {
                let (selection, speed_mode) = normalized_speed_default(selection, speed_mode);
                self.default_smart_reasoning_level = reasoning_level;
                self.default_smart_selection = selection;
                self.default_smart_speed_mode = speed_mode;
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
            SettingsOperation::OrchestrationParallelism(value) => {
                self.orchestration_parallelism = value.clamp(1, MAX_ORCHESTRATION_PARALLELISM);
                self.persist_orchestration_parallelism_setting().await;
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

    /// Persists the default response style used by new sessions.
    async fn persist_default_response_style_setting(&self) {
        let _ = self
            .repositories
            .settings()
            .upsert_project_setting(
                self.project_id,
                SettingName::DefaultResponseStyle,
                self.default_response_style.as_str(),
            )
            .await;
    }

    /// Persists whether research-only orchestration waves start immediately.
    async fn persist_auto_approve_orchestration_research_setting(&self) {
        let value = self.auto_approve_orchestration_research.to_string();

        let _ = self
            .repositories
            .settings()
            .upsert_setting(SettingName::AutoApproveOrchestrationResearch, &value)
            .await;
    }

    /// Persists the global orchestration concurrency cap.
    async fn persist_orchestration_parallelism_setting(&self) {
        let value = self.orchestration_parallelism.to_string();

        // Best-effort: settings persistence failure is non-critical.
        let _ = self
            .repositories
            .settings()
            .upsert_setting(SettingName::OrchestrationParallelism, &value)
            .await;
    }

    /// Atomically persists smart-model selector values (`DefaultSmartAgent`,
    /// `DefaultSmartModel`, `DefaultSmartReasoningLevel`,
    /// `DefaultSmartSpeedMode`, and
    /// `LastUsedModelAsDefault`).
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
                        SettingName::DefaultSmartReasoningLevel,
                        self.default_smart_reasoning_level.as_str().to_string(),
                    ),
                    (
                        SettingName::DefaultSmartSpeedMode,
                        self.default_smart_speed_mode.as_str().to_string(),
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

    /// Atomically persists the fast-model selector values (`DefaultFastAgent`,
    /// `DefaultFastModel`, `DefaultFastReasoningLevel`, and
    /// `DefaultFastSpeedMode`).
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
                    (
                        SettingName::DefaultFastReasoningLevel,
                        self.default_fast_reasoning_level.as_str().to_string(),
                    ),
                    (
                        SettingName::DefaultFastSpeedMode,
                        self.default_fast_speed_mode.as_str().to_string(),
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
    /// (`DefaultReviewAgent`, `DefaultReviewModel`, and
    /// `DefaultReviewReasoningLevel`, and `DefaultReviewSpeedMode`).
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
                    (
                        SettingName::DefaultReviewReasoningLevel,
                        self.default_review_reasoning_level.as_str().to_string(),
                    ),
                    (
                        SettingName::DefaultReviewSpeedMode,
                        self.default_review_speed_mode.as_str().to_string(),
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

/// Normalizes one role default so unsupported providers cannot retain Fast
/// and supported providers use a model accepted by their Fast transport.
fn normalized_speed_default(
    selection: AgentSelection,
    speed_mode: SpeedMode,
) -> (AgentSelection, SpeedMode) {
    if !selection.kind().supports_speed_mode() {
        return (selection, SpeedMode::Normal);
    }

    (selection.compatible_with_speed_mode(speed_mode), speed_mode)
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

/// Loads one project-scoped model setting and its owning agent setting.
///
/// Older projects may only have a model key; in that case the fallback
/// selection and available-provider resolution decide ownership. Retired
/// model ids are atomically replaced before runtime availability fallback is
/// applied. Explicit providers are preserved, while ambiguous model-only
/// settings use an available provider that supports the replacement.
async fn load_model_selection_setting(
    repositories: &AppRepositories,
    project_id: Option<i64>,
    agent_setting_name: SettingName,
    model_setting_name: SettingName,
    fallback_selection: AgentSelection,
    available_agent_kinds: &[AgentKind],
) -> Option<AgentSelection> {
    let project_id = project_id?;
    let setting_value = repositories
        .settings()
        .get_project_setting(project_id, model_setting_name)
        .await
        .unwrap_or(None)?;
    let is_retired = AgentModel::retired_replacement(&setting_value).is_some();
    let model = AgentModel::parse_persisted(&setting_value).ok()?;
    let persisted_agent_kind =
        load_agent_setting(repositories, Some(project_id), agent_setting_name)
            .await
            .filter(|agent_kind| agent_kind.supports_model(model));
    let agent_kind = persisted_agent_kind
        .unwrap_or_else(|| fallback_agent_kind_for_model(model, fallback_selection.kind()));
    let persisted_selection = AgentSelection::new(agent_kind, model);
    let resolved_selection =
        resolve_available_selection(persisted_selection, available_agent_kinds);

    if is_retired {
        let replacement_agent_kind = persisted_agent_kind.unwrap_or_else(|| {
            if resolved_selection.kind().supports_model(model) {
                resolved_selection.kind()
            } else {
                agent_kind
            }
        });

        let _ = repositories
            .settings()
            .upsert_project_settings(
                project_id,
                vec![
                    (model_setting_name, model.as_str().to_string()),
                    (
                        agent_setting_name,
                        replacement_agent_kind.name().to_string(),
                    ),
                ],
            )
            .await;
    }

    Some(resolved_selection)
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

/// Returns the owner-independent smart-model baseline for one provider set.
fn default_smart_fallback_selection(available_agent_kinds: &[AgentKind]) -> AgentSelection {
    fallback_selection_for_available_model(
        AgentKind::Antigravity.default_model(),
        available_agent_kinds,
    )
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

/// Loads the project response style, falling back when storage is unavailable.
async fn load_default_response_style_setting_from_repositories(
    repositories: &AppRepositories,
    project_id: i64,
) -> ResponseStyle {
    repositories
        .settings()
        .load_project_response_style(project_id, SettingName::DefaultResponseStyle)
        .await
        .unwrap_or_default()
}

/// Loads and bounds the global orchestration concurrency cap.
async fn load_orchestration_parallelism_setting_from_repositories(
    repositories: &AppRepositories,
) -> u8 {
    repositories
        .settings()
        .get_setting(SettingName::OrchestrationParallelism)
        .await
        .unwrap_or(None)
        .and_then(|setting_value| setting_value.parse::<u8>().ok())
        .unwrap_or(DEFAULT_ORCHESTRATION_PARALLELISM)
        .clamp(1, MAX_ORCHESTRATION_PARALLELISM)
}

async fn load_auto_approve_orchestration_research_setting_from_repositories(
    repositories: &AppRepositories,
) -> bool {
    repositories
        .settings()
        .get_setting(SettingName::AutoApproveOrchestrationResearch)
        .await
        .unwrap_or(None)
        .and_then(|setting_value| setting_value.parse::<bool>().ok())
        .unwrap_or(DEFAULT_AUTO_APPROVE_ORCHESTRATION_RESEARCH)
}

#[cfg(test)]
#[path = "setting_test.rs"]
mod tests;
