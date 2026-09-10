use super::*;

#[test]
/// Ensures Fast compatibility is exact and selects required fallback
/// models without changing Normal selections.
fn test_agent_selection_speed_compatibility() {
    // Arrange
    let cases = [
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeFable5),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5),
            false,
        ),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt53CodexSpark),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            false,
        ),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Astra),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Astra),
            true,
        ),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Terra),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Terra),
            true,
        ),
        (
            AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31Pro),
            SpeedMode::Fast,
            AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31Pro),
            false,
        ),
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeFable5),
            SpeedMode::Normal,
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeFable5),
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
            AgentModel::Gpt56Sol,
            AgentModel::Gpt56Terra,
            AgentModel::Gpt56Luna,
            AgentModel::Gpt53CodexSpark,
            AgentModel::Gemini31Pro,
            AgentModel::Gemini38Flash,
            AgentModel::Gemini35FlashLite,
        ]
    );
}

#[test]
/// Ensures shared Gemini models appear once even when both Google
/// providers are available.
fn test_selectable_models_for_agent_kinds_deduplicates_shared_models() {
    // Arrange
    let agent_kinds = [AgentKind::Gemini, AgentKind::Antigravity];

    // Act
    let selectable_models = selectable_models_for_agent_kinds(&agent_kinds);

    // Assert
    assert_eq!(
        selectable_models,
        vec![
            AgentModel::Gemini31Pro,
            AgentModel::Gemini38Flash,
            AgentModel::Gemini35FlashLite,
        ]
    );
}

#[test]
/// Ensures unavailable models fall back to an available preferred model
/// when possible.
fn test_resolve_model_for_available_agent_kinds_prefers_available_fallback() {
    // Arrange
    let unavailable_model = AgentModel::ClaudeOpus5;
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
    let unavailable_model = AgentModel::ClaudeOpus5;
    let available_agent_kinds = [AgentKind::Codex, AgentKind::Antigravity];
    let unavailable_fallback_model = AgentModel::ClaudeSonnet5;

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
    let model = AgentModel::Gemini38Flash;
    let available_agent_kinds = [AgentKind::Gemini, AgentKind::Antigravity];

    // Act
    let resolved_selection =
        resolve_agent_selection_for_model(model, AgentKind::Antigravity, &available_agent_kinds);

    // Assert
    assert_eq!(
        resolved_selection,
        AgentSelection::new(AgentKind::Antigravity, model)
    );
}

#[test]
/// Ensures model-only settings use available provider order when the
/// preferred provider cannot run the selected model.
fn test_resolve_agent_selection_for_model_uses_available_provider_order() {
    // Arrange
    let model = AgentModel::Gemini38Flash;
    let available_agent_kinds = [AgentKind::Gemini, AgentKind::Antigravity];

    // Act
    let resolved_selection =
        resolve_agent_selection_for_model(model, AgentKind::Codex, &available_agent_kinds);

    // Assert
    assert_eq!(
        resolved_selection,
        AgentSelection::new(AgentKind::Gemini, model)
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
