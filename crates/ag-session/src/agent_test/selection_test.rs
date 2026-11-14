use ag_contracts::SpeedMode;

use crate::agent::{
    AgentKind, AgentModel, AgentSelection, resolve_agent_selection_for_model,
    resolve_model_for_available_agent_kinds, resolve_prompt_model_agent_kind,
    selectable_models_for_agent_kinds,
};

#[test]
/// Ensures Fast compatibility is exact and selects required fallback
/// models without changing Normal selections.
fn test_agent_selection_speed_compatibility() {
    // Arrange
    let cases = [
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeFable52),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55),
            false,
        ),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt53CodexSpark),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            false,
        ),
        (
            AgentSelection::new(AgentKind::Harness, AgentModel::KimiK3),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Harness, AgentModel::KimiK3),
            false,
        ),
        (
            AgentSelection::new(AgentKind::Harness, AgentModel::Gpt61Sol),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Harness, AgentModel::Gpt61Sol),
            false,
        ),
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55),
            true,
        ),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Astra),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Astra),
            true,
        ),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            true,
        ),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Luna),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Luna),
            true,
        ),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Terra),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Terra),
            true,
        ),
        (
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini4Argon),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini4Argon),
            false,
        ),
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeFable52),
            SpeedMode::Normal,
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeFable52),
            false,
        ),
    ];

    // Act / Assert
    for (selection, speed_mode, expected_selection, supports_fast_mode) in cases {
        assert_eq!(
            selection.compatible_with_speed_mode(speed_mode),
            expected_selection
        );
        assert_eq!(selection.supports_fast_mode(), supports_fast_mode);
    }
}

#[test]
/// Ensures selectable-model ordering follows the provided provider order.
fn test_selectable_models_for_agent_kinds_uses_provider_order() {
    // Arrange
    let agent_kinds = [AgentKind::Codex, AgentKind::Antigravity];

    // Act
    let selectable_models = selectable_models_for_agent_kinds(&agent_kinds);

    // Assert
    assert_eq!(
        selectable_models,
        vec![
            AgentModel::Gpt6Astra,
            AgentModel::Gpt61Sol,
            AgentModel::Gpt6Luna,
            AgentModel::Gpt56Terra,
            AgentModel::Gpt53CodexSpark,
            AgentModel::Gemini4Argon,
            AgentModel::Gemini4Hydrogen,
            AgentModel::GeminiProOrchestrator,
            AgentModel::Gemini38Flash,
            AgentModel::ClaudeOpus55,
            AgentModel::ClaudeSonnet55,
            AgentModel::GptOss120b,
        ]
    );
}

#[test]
/// Ensures Codex models shared with the harness appear once even when both
/// providers are available.
fn test_selectable_models_for_agent_kinds_deduplicates_shared_models() {
    // Arrange
    let agent_kinds = [AgentKind::Harness, AgentKind::Codex];
    let mut expected_models = AgentKind::Harness.models().to_vec();
    expected_models.push(AgentModel::Gpt53CodexSpark);

    // Act
    let selectable_models = selectable_models_for_agent_kinds(&agent_kinds);

    // Assert
    assert_eq!(selectable_models, expected_models);
}

#[test]
/// Ensures unavailable models fall back to an available preferred model
/// when possible.
fn test_resolve_model_for_available_agent_kinds_prefers_available_fallback() {
    // Arrange
    let unavailable_model = AgentModel::ClaudeFable52;
    let available_agent_kinds = [AgentKind::Codex, AgentKind::Antigravity];
    let fallback_model = AgentModel::Gpt56Terra;

    // Act
    let resolved_model = resolve_model_for_available_agent_kinds(
        unavailable_model,
        &available_agent_kinds,
        fallback_model,
    );

    // Assert
    assert_eq!(resolved_model, AgentModel::Gpt56Terra);
}

#[test]
/// Ensures unavailable models fall back to the first available provider
/// default when the preferred fallback is also unavailable.
fn test_resolve_model_for_available_agent_kinds_uses_first_available_default() {
    // Arrange
    let unavailable_model = AgentModel::ClaudeFable52;
    let available_agent_kinds = [AgentKind::Codex, AgentKind::Antigravity];
    let unavailable_fallback_model = AgentModel::ClaudeHaiku55;

    // Act
    let resolved_model = resolve_model_for_available_agent_kinds(
        unavailable_model,
        &available_agent_kinds,
        unavailable_fallback_model,
    );

    // Assert
    assert_eq!(resolved_model, AgentKind::Codex.default_model());
}

#[test]
/// Ensures model-only settings preserve a preferred provider when it can
/// run the selected model.
fn test_resolve_agent_selection_for_model_preserves_preferred_shared_provider() {
    // Arrange
    let model = AgentModel::Gpt61Sol;
    let available_agent_kinds = [AgentKind::Codex, AgentKind::Harness];

    // Act
    let resolved_selection =
        resolve_agent_selection_for_model(model, AgentKind::Harness, &available_agent_kinds);

    // Assert
    assert_eq!(
        resolved_selection,
        AgentSelection::new(AgentKind::Harness, model)
    );
}

#[test]
/// Ensures model-only settings move to the model's owning provider when the
/// preferred provider cannot run it, regardless of available provider order.
fn test_resolve_agent_selection_for_model_prefers_available_owner() {
    // Arrange
    let shared_models = [AgentModel::Gpt61Sol, AgentModel::ClaudeOpus55];
    let available_agent_kinds = [
        AgentKind::Antigravity,
        AgentKind::Harness,
        AgentKind::Claude,
        AgentKind::Codex,
    ];

    // Act
    let resolved_selections = shared_models.map(|model| {
        resolve_agent_selection_for_model(model, AgentKind::Codex, &available_agent_kinds)
    });

    // Assert
    assert_eq!(
        resolved_selections,
        [
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55),
        ]
    );
}

#[test]
/// Ensures model-only settings use available provider order when neither
/// the preferred nor the owning provider can run the selected model.
fn test_resolve_agent_selection_for_model_uses_available_provider_order() {
    // Arrange
    let model = AgentModel::ClaudeSonnet55;
    let available_agent_kinds = [AgentKind::Codex, AgentKind::Antigravity];

    // Act
    let resolved_selection =
        resolve_agent_selection_for_model(model, AgentKind::Codex, &available_agent_kinds);

    // Assert
    assert_eq!(
        resolved_selection,
        AgentSelection::new(AgentKind::Antigravity, model)
    );
}

#[test]
/// Ensures prompt model selection keeps the current backend when it is
/// still available locally.
fn test_resolve_prompt_model_agent_kind_prefers_current_agent() {
    // Arrange
    let available_agent_kinds = [AgentKind::Antigravity, AgentKind::Codex];

    // Act
    let resolved_agent_kind =
        resolve_prompt_model_agent_kind(AgentKind::Codex, &available_agent_kinds);

    // Assert
    assert_eq!(resolved_agent_kind, Some(AgentKind::Codex));
}

#[test]
/// Ensures prompt model selection falls back to the first locally
/// available backend when the current backend is unavailable.
fn test_resolve_prompt_model_agent_kind_uses_first_available_agent() {
    // Arrange
    let available_agent_kinds = [AgentKind::Antigravity, AgentKind::Codex];

    // Act
    let resolved_agent_kind =
        resolve_prompt_model_agent_kind(AgentKind::Claude, &available_agent_kinds);

    // Assert
    assert_eq!(resolved_agent_kind, Some(AgentKind::Antigravity));
}

#[test]
fn test_available_selected_model_is_preserved() {
    // Arrange
    let selected = AgentModel::Gpt6Astra;

    // Act
    let resolved = resolve_model_for_available_agent_kinds(
        selected,
        &[AgentKind::Codex],
        AgentModel::Gpt61Sol,
    );

    // Assert
    assert_eq!(resolved, selected);
}
