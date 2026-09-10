use crate::model::agent::{
    AgentKind, AgentModel, AgentSelection, parse_persisted_session_agent_model,
};

#[test]
/// Ensures the retirement registry maps retired ids to replacements and
/// leaves current ids unmapped.
fn test_retired_replacement_maps_only_retired_ids() {
    // Arrange
    let retired_ids = [
        ("gemini-3-pro-preview", AgentModel::Gemini31Pro),
        ("gemini-3.1-pro", AgentModel::Gemini31Pro),
        ("gemini-3-flash-preview", AgentModel::Gemini38Flash),
        ("gemini-3.7-flash", AgentModel::Gemini38Flash),
        ("gemini-3.6-flash", AgentModel::Gemini38Flash),
        ("gemini-3.5-flash", AgentModel::Gemini35FlashLite),
        (
            "gemini-3.1-flash-lite-preview",
            AgentModel::Gemini35FlashLite,
        ),
        ("claude-opus-4-8", AgentModel::ClaudeOpus5),
        ("claude-opus-4-6", AgentModel::ClaudeOpus5),
        ("claude-opus-4-7", AgentModel::ClaudeOpus5),
        ("claude-sonnet-4-6", AgentModel::ClaudeSonnet5),
        ("gpt-5.5", AgentModel::Gpt56Sol),
        ("gpt-5.4-mini", AgentModel::Gpt56Luna),
        ("gpt-5.4", AgentModel::Gpt56Sol),
        ("gpt-5.3-codex", AgentModel::Gpt56Sol),
        ("gpt-5.2-codex", AgentModel::Gpt53CodexSpark),
    ];

    // Act
    let replacements =
        retired_ids.map(|(retired_id, _)| AgentModel::retired_replacement(retired_id));
    let selectable_parses = retired_ids.map(|(retired_id, _)| retired_id.parse::<AgentModel>());
    let current_replacements = [
        "gemini-3.1-pro-preview",
        "gemini-3.8-flash",
        "gemini-3.5-flash-lite",
        "claude-opus-5",
    ]
    .map(AgentModel::retired_replacement);
    let unknown_replacement = AgentModel::retired_replacement("not-a-model");

    // Assert
    assert_eq!(
        replacements,
        retired_ids.map(|(_, replacement)| Some(replacement))
    );
    assert!(selectable_parses.iter().all(Result::is_err));
    assert_eq!(current_replacements, [None; 4]);
    assert_eq!(unknown_replacement, None);
}

#[test]
/// Ensures persisted model ids parse or migrate to supported
/// models.
fn test_parse_persisted_handles_supported_and_retired_models() {
    // Arrange

    // Act
    let parsed_opus_46 = AgentModel::parse_persisted("claude-opus-4-6");
    let parsed_opus_47 = AgentModel::parse_persisted("claude-opus-4-7");
    let parsed_sonnet_46 = AgentModel::parse_persisted("claude-sonnet-4-6");
    let parsed_sonnet_5 = AgentModel::parse_persisted("claude-sonnet-5");
    let parsed_gpt_54_mini = AgentModel::parse_persisted("gpt-5.4-mini");
    let parsed_gpt_54 = AgentModel::parse_persisted("gpt-5.4");
    let parsed_gemini_38_flash = AgentModel::parse_persisted("gemini-3.8-flash");
    let parsed_gemini_37_flash = AgentModel::parse_persisted("gemini-3.7-flash");
    let parsed_gemini_36_flash = AgentModel::parse_persisted("gemini-3.6-flash");
    let parsed_gemini_35_flash = AgentModel::parse_persisted("gemini-3.5-flash");
    let parsed_gemini_3_flash_preview = AgentModel::parse_persisted("gemini-3-flash-preview");
    let parsed_gemini_35_flash_lite = AgentModel::parse_persisted("gemini-3.5-flash-lite");
    let parsed_gemini_31_pro = AgentModel::parse_persisted("gemini-3.1-pro");
    let parsed_gemini_31_pro_preview = AgentModel::parse_persisted("gemini-3.1-pro-preview");
    let parsed_gemini_31_flash_lite_preview =
        AgentModel::parse_persisted("gemini-3.1-flash-lite-preview");

    // Assert
    assert_eq!(parsed_opus_46, Ok(AgentModel::ClaudeOpus5));
    assert_eq!(parsed_opus_47, Ok(AgentModel::ClaudeOpus5));
    assert_eq!(parsed_sonnet_46, Ok(AgentModel::ClaudeSonnet5));
    assert_eq!(parsed_sonnet_5, Ok(AgentModel::ClaudeSonnet5));
    assert_eq!(parsed_gpt_54_mini, Ok(AgentModel::Gpt56Luna));
    assert_eq!(parsed_gpt_54, Ok(AgentModel::Gpt56Sol));
    assert_eq!(parsed_gemini_38_flash, Ok(AgentModel::Gemini38Flash));
    assert_eq!(parsed_gemini_37_flash, Ok(AgentModel::Gemini38Flash));
    assert_eq!(parsed_gemini_36_flash, Ok(AgentModel::Gemini38Flash));
    assert_eq!(parsed_gemini_35_flash, Ok(AgentModel::Gemini35FlashLite));
    assert_eq!(parsed_gemini_3_flash_preview, Ok(AgentModel::Gemini38Flash));
    assert_eq!(
        parsed_gemini_35_flash_lite,
        Ok(AgentModel::Gemini35FlashLite)
    );
    assert_eq!(parsed_gemini_31_pro, Ok(AgentModel::Gemini31Pro));
    assert_eq!(parsed_gemini_31_pro_preview, Ok(AgentModel::Gemini31Pro));
    assert_eq!(
        parsed_gemini_31_flash_lite_preview,
        Ok(AgentModel::Gemini35FlashLite)
    );
}

#[test]
/// Ensures persisted session agent values constrain model parsing instead
/// of deriving provider ownership from the model string.
fn test_parse_persisted_session_agent_model_prefers_saved_agent() {
    // Arrange

    // Act
    let selection = parse_persisted_session_agent_model(Some("codex"), "gemini-3.8-flash");

    // Assert
    assert_eq!(selection.kind(), AgentKind::Codex);
    assert_eq!(selection.model(), AgentKind::Codex.default_model());
}

#[test]
/// Ensures Antigravity-persisted sessions keep Antigravity ownership for
/// raw Gemini model ids shared with the direct Gemini backend.
fn test_parse_persisted_session_agent_model_preserves_antigravity_models() {
    // Arrange

    // Act
    let selection = parse_persisted_session_agent_model(Some("antigravity"), "gemini-3.8-flash");

    // Assert
    assert_eq!(selection.kind(), AgentKind::Antigravity);
    assert_eq!(selection.model(), AgentModel::Gemini38Flash);
}

#[test]
/// Ensures current and retired Gemini models preserve the saved Google
/// provider while resolving to supported models.
fn test_parse_persisted_session_agent_model_resolves_saved_google_models() {
    // Arrange
    let persisted_selections = [
        ("gemini", "gemini-3.5-flash-lite"),
        ("gemini", "gemini-3.5-flash"),
        ("antigravity", "gemini-3.5-flash-lite"),
        ("antigravity", "gemini-3.5-flash"),
    ];

    // Act
    let migrated_selections = persisted_selections.map(|(agent_value, model_value)| {
        parse_persisted_session_agent_model(Some(agent_value), model_value)
    });

    // Assert
    assert_eq!(
        migrated_selections,
        [
            AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini35FlashLite),
            AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini35FlashLite),
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini35FlashLite),
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini35FlashLite),
        ]
    );
}

#[test]
/// Ensures older persisted rows without `agent` still load through the
/// legacy model-derived compatibility path.
fn test_parse_persisted_session_agent_model_supports_legacy_rows() {
    // Arrange

    // Act
    let selection = parse_persisted_session_agent_model(None, "claude-opus-4-6");

    // Assert
    assert_eq!(selection.kind(), AgentKind::Claude);
    assert_eq!(selection.model(), AgentModel::ClaudeOpus5);
}

#[test]
/// Ensures older `gemini-*` rows without `agent` keep the post-removal
/// Antigravity compatibility default.
fn test_parse_persisted_session_agent_model_defaults_legacy_gemini_to_antigravity() {
    // Arrange

    // Act
    let selection = parse_persisted_session_agent_model(None, "gemini-3.8-flash");

    // Assert
    assert_eq!(selection.kind(), AgentKind::Antigravity);
    assert_eq!(selection.model(), AgentModel::Gemini38Flash);
}

#[test]
/// Ensures current and retired Gemini models in rows without a saved
/// provider resolve through the legacy Antigravity compatibility path.
fn test_parse_persisted_session_agent_model_resolves_legacy_gemini_rows() {
    // Arrange

    // Act
    let migrated_flash = parse_persisted_session_agent_model(None, "gemini-3.5-flash");
    let loaded_flash_lite = parse_persisted_session_agent_model(None, "gemini-3.5-flash-lite");

    // Assert
    assert_eq!(
        migrated_flash,
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini35FlashLite)
    );
    assert_eq!(
        loaded_flash_lite,
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini35FlashLite)
    );
}
