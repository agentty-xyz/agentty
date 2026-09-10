use ag_agent::AgentSelectionMetadata;

use super::super::{
    ModelSelectorOption, SettingsManager, load_default_fast_agent_setting,
    load_default_smart_agent_setting, load_default_smart_model_setting, normalized_speed_default,
    selectable_model_options,
};
use super::support::{
    assert_role_defaults_persisted, new_settings_manager, select_row, settings_manager,
    test_services, test_services_with_available_agent_kinds,
};
use crate::app::AppServices;
use crate::db::AppRepositories;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::setting::SettingName;
use crate::domain::theme::ColorTheme;
use crate::presentation::setting::SettingsOperation;

#[test]
fn speed_defaults_normalize_provider_support_and_fast_model_compatibility() {
    // Arrange
    let codex_spark = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt53CodexSpark);
    let gemini = AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31Pro);

    // Act
    let fast_codex = normalized_speed_default(codex_spark, SpeedMode::Fast);
    let fast_gemini = normalized_speed_default(gemini, SpeedMode::Fast);
    let normal_codex = normalized_speed_default(codex_spark, SpeedMode::Normal);

    // Assert
    assert_eq!(
        fast_codex,
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            SpeedMode::Fast,
        )
    );
    assert_eq!(fast_gemini, (gemini, SpeedMode::Normal));
    assert_eq!(normal_codex, (codex_spark, SpeedMode::Normal));
}

#[test]
fn settings_rows_include_role_model_coauthor_and_launch_configuration_options() {
    // Arrange
    let manager = new_settings_manager();

    // Act
    let rows = manager.settings_rows();

    // Assert
    assert_eq!(rows.len(), 9);
    assert_eq!(rows[0].0, "Theme");
    assert_eq!(rows[1].0, "Orchestrator Parallelism");
    assert_eq!(rows[2].0, "Auto-approve Research");
    assert_eq!(rows[2].1, "Enabled");
    assert_eq!(rows[3].0, "Default Smart Model");
    assert_eq!(rows[4].0, "Default Fast Model");
    assert_eq!(rows[5].0, "Default Review Model");
    assert_eq!(rows[6].0, "Coauthored by Agentty");
    assert_eq!(rows[7].0, "Launch Configurations");
    assert_eq!(rows[8].0, "Default Response Style");
}

#[test]
fn settings_rows_show_empty_placeholder_for_launch_configuration() {
    // Arrange
    let manager = new_settings_manager();

    // Act
    let rows = manager.settings_rows();

    // Assert
    assert_eq!(rows[7].1, "(none)");
}

#[test]
fn settings_rows_show_single_launch_configuration_summary() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().launch_configuration = "http://localhost:5173".to_string();

    // Act
    let rows = manager.settings_rows();

    // Assert
    assert_eq!(rows[7].1, "http://localhost:5173");
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
    assert_eq!(rows[7].1, "cargo test (+2 more)");
}

#[test]
fn settings_rows_show_last_used_model_as_default_value_when_enabled() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().use_last_used_model_as_default = true;

    // Act
    let rows = manager.settings_rows();

    // Assert
    assert_eq!(rows[3].1, "Last used model as default [high]");
}

#[test]
fn settings_rows_show_default_smart_model_with_agent_prefix() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().default_smart_selection =
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini31Pro);

    // Act
    let rows = manager.settings_rows();

    // Assert
    assert_eq!(rows[3].1, "antigravity/gemini-3.1-pro-preview [high]");
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
    view.default_smart_selection = AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31Pro);

    // Act
    let rows = manager.settings_rows();

    // Assert
    assert_eq!(rows[3].1, "gemini/gemini-3.1-pro-preview [high]");
}

#[test]
fn settings_rows_show_default_fast_model_value() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().default_fast_selection =
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol);

    // Act
    let rows = manager.settings_rows();

    // Assert
    assert_eq!(rows[4].1, "codex/gpt-5.6-sol [low, Normal]");
}

#[test]
fn settings_rows_show_coauthored_by_agentty_value() {
    // Arrange
    let mut manager = new_settings_manager();
    manager.fixture_view_mut().include_coauthored_by_agentty = false;

    // Act
    let rows = manager.settings_rows();

    // Assert
    assert_eq!(rows[6].1, "Disabled");
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

#[tokio::test]
async fn settings_manager_new_persists_replacements_for_retired_model_defaults() {
    // Arrange
    let available_agent_kinds = vec![AgentKind::Codex, AgentKind::Antigravity, AgentKind::Claude];
    let (services, project_id) =
        test_services_with_available_agent_kinds(available_agent_kinds).await;
    services
        .db()
        .settings()
        .upsert_project_settings(
            project_id,
            vec![
                (SettingName::DefaultSmartModel, "gemini-3.1-pro".to_string()),
                (
                    SettingName::DefaultFastModel,
                    "gemini-3-flash-preview".to_string(),
                ),
                (
                    SettingName::DefaultReviewModel,
                    "claude-opus-4-6".to_string(),
                ),
            ],
        )
        .await
        .expect("failed to persist retired model defaults");

    // Act
    let manager = settings_manager(&services, project_id).await;
    let settings = manager.settings();

    // Assert
    assert_eq!(
        settings.default_smart_selection,
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini31Pro)
    );
    assert_eq!(
        settings.default_fast_selection,
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash)
    );
    assert_eq!(
        settings.default_review_selection,
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5)
    );

    let expected_persisted_settings = [
        (
            SettingName::DefaultSmartModel,
            AgentModel::Gemini31Pro.as_str(),
        ),
        (
            SettingName::DefaultSmartAgent,
            AgentKind::Antigravity.name(),
        ),
        (
            SettingName::DefaultFastModel,
            AgentModel::Gemini38Flash.as_str(),
        ),
        (SettingName::DefaultFastAgent, AgentKind::Antigravity.name()),
        (
            SettingName::DefaultReviewModel,
            AgentModel::ClaudeOpus5.as_str(),
        ),
        (SettingName::DefaultReviewAgent, AgentKind::Claude.name()),
    ];

    for (setting_name, expected_value) in expected_persisted_settings {
        assert_eq!(
            services
                .db()
                .settings()
                .get_project_setting(project_id, setting_name)
                .await
                .expect("failed to load migrated project setting"),
            Some(expected_value.to_string())
        );
    }
}

#[tokio::test]
async fn settings_manager_preserves_retired_default_when_provider_is_unavailable() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("db should open");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    repositories
        .settings()
        .upsert_project_setting(project_id, SettingName::DefaultSmartModel, "gpt-5.5")
        .await
        .expect("failed to persist retired smart model");

    // Act
    let unavailable_manager = SettingsManager::from_repositories(
        repositories.clone(),
        vec![AgentKind::Claude],
        project_id,
    )
    .await;
    let persisted_model = repositories
        .settings()
        .get_project_setting(project_id, SettingName::DefaultSmartModel)
        .await
        .expect("failed to load migrated smart model");
    let persisted_agent = repositories
        .settings()
        .get_project_setting(project_id, SettingName::DefaultSmartAgent)
        .await
        .expect("failed to load migrated smart agent");

    // Assert
    assert_eq!(
        unavailable_manager.default_smart_selection,
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeFable5)
    );
    assert_eq!(
        persisted_model.as_deref(),
        Some(AgentModel::Gpt56Sol.as_str())
    );
    assert_eq!(persisted_agent.as_deref(), Some(AgentKind::Codex.name()));

    // Act
    let available_manager = SettingsManager::from_repositories(
        repositories,
        vec![AgentKind::Codex, AgentKind::Claude],
        project_id,
    )
    .await;

    // Assert
    assert_eq!(
        available_manager.default_smart_selection,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol)
    );
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
async fn selector_dropdown_selects_coauthor_setting_and_persists_value() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager(&services, project_id).await;
    select_row(&mut manager, 6);

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
    assert_eq!(dropdown.options.len(), 3);
    assert_eq!(dropdown.options[1].label, "Agentty Green");
    assert_eq!(dropdown.options[2].label, "Dark Horizon");

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
async fn selector_dropdown_persists_last_used_flag_and_explicit_smart_model() {
    // Arrange
    let (services, project_id) = test_services().await;
    let options = selectable_model_options(AgentKind::ALL);
    let last_option = *options.last().expect("model options should not be empty");
    services
        .db()
        .settings()
        .upsert_project_settings(
            project_id,
            vec![
                (
                    SettingName::DefaultSmartAgent,
                    last_option.agent_kind.name().to_string(),
                ),
                (
                    SettingName::DefaultSmartModel,
                    last_option.model.as_str().to_string(),
                ),
                (SettingName::LastUsedModelAsDefault, "false".to_string()),
                (
                    SettingName::DefaultSmartReasoningLevel,
                    ReasoningLevel::Max.as_str().to_string(),
                ),
            ],
        )
        .await
        .expect("failed to persist smart selector fixture");
    let mut manager = settings_manager(&services, project_id).await;
    select_row(&mut manager, 3);

    // Act
    manager.handle_enter();
    let dropdown = manager
        .selector_dropdown()
        .expect("expected smart model selector dropdown");
    assert_eq!(dropdown.title, "Select model");
    assert_eq!(dropdown.selected_index, options.len() - 1);
    manager.next_selector_dropdown_option();
    manager.select_selector_dropdown_option().await;
    manager.next_selector_dropdown_option();
    manager.select_selector_dropdown_option().await;
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
    assert_eq!(
        services
            .db()
            .settings()
            .load_project_reasoning_level(project_id, SettingName::DefaultSmartReasoningLevel,)
            .await
            .expect("failed to load smart reasoning level"),
        ReasoningLevel::Low
    );

    // Act
    manager.handle_enter();
    manager.next_selector_dropdown_option();
    manager.select_selector_dropdown_option().await;
    manager.select_selector_dropdown_option().await;
    manager.select_selector_dropdown_option().await;

    // Assert
    assert!(!manager.settings().use_last_used_model_as_default);
    assert_eq!(
        manager.settings().default_smart_selection,
        options[0].selection()
    );
    assert_eq!(
        manager.settings().default_smart_reasoning_level,
        ReasoningLevel::Low
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
async fn apply_operation_persists_role_model_reasoning_and_speed_settings() {
    // Arrange
    let (services, project_id) = test_services().await;
    let mut manager = settings_manager(&services, project_id).await;
    let fast_selection = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol);
    let review_selection = AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5);

    // Act
    manager
        .persist_operation(SettingsOperation::DefaultFastSelection {
            reasoning_level: ReasoningLevel::Low,
            selection: fast_selection,
            speed_mode: SpeedMode::Fast,
        })
        .await;
    manager
        .persist_operation(SettingsOperation::DefaultReviewSelection {
            reasoning_level: ReasoningLevel::XHigh,
            selection: review_selection,
            speed_mode: SpeedMode::Fast,
        })
        .await;

    // Assert
    assert_eq!(manager.settings().default_fast_selection, fast_selection);
    assert_eq!(
        manager.settings().default_review_selection,
        review_selection
    );
    assert_eq!(
        manager.settings().default_fast_reasoning_level,
        ReasoningLevel::Low
    );
    assert_eq!(
        manager.settings().default_review_reasoning_level,
        ReasoningLevel::XHigh
    );
    assert_eq!(manager.settings().default_fast_speed_mode, SpeedMode::Fast);
    assert_eq!(
        manager.settings().default_review_speed_mode,
        SpeedMode::Fast
    );
    assert_role_defaults_persisted(
        &services,
        project_id,
        (
            SettingName::DefaultFastAgent,
            SettingName::DefaultFastModel,
            SettingName::DefaultFastReasoningLevel,
            SettingName::DefaultFastSpeedMode,
        ),
        (fast_selection, ReasoningLevel::Low, SpeedMode::Fast),
    )
    .await;
    assert_role_defaults_persisted(
        &services,
        project_id,
        (
            SettingName::DefaultReviewAgent,
            SettingName::DefaultReviewModel,
            SettingName::DefaultReviewReasoningLevel,
            SettingName::DefaultReviewSpeedMode,
        ),
        (review_selection, ReasoningLevel::XHigh, SpeedMode::Fast),
    )
    .await;
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
fn setting_name_as_str_returns_default_role_reasoning_levels() {
    // Arrange

    // Act
    let setting_names = [
        SettingName::DefaultSmartReasoningLevel.as_str(),
        SettingName::DefaultFastReasoningLevel.as_str(),
        SettingName::DefaultReviewReasoningLevel.as_str(),
    ];

    // Assert
    assert_eq!(
        setting_names,
        [
            "DefaultSmartReasoningLevel",
            "DefaultFastReasoningLevel",
            "DefaultReviewReasoningLevel",
        ]
    );
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
            AgentModel::Gemini31Pro.as_str(),
        )
        .await
        .expect("failed to persist smart model");

    // Act
    let loaded_selection = load_default_smart_agent_setting(
        &services,
        Some(project_id),
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
    )
    .await;

    // Assert
    assert_eq!(
        loaded_selection,
        AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31Pro)
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
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5)
    );

    // Arrange
    services
        .db()
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultFastModel,
            AgentModel::Gpt56Sol.as_str(),
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
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol)
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
            AgentModel::Gemini31Pro.as_str(),
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
