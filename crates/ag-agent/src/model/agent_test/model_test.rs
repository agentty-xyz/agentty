use crate::model::agent::{AgentKind, AgentModel, AgentSelectionMetadata};

#[test]
/// Ensures model parsing is constrained to the selected provider.
fn test_parse_model_returns_none_for_models_from_other_providers() {
    // Arrange
    let claude_kind = AgentKind::Claude;
    let antigravity_model = AgentModel::Gemini38Flash.as_str();

    // Act
    let parsed_model = claude_kind.parse_model(antigravity_model);

    // Assert
    assert_eq!(parsed_model, None);
}

#[test]
/// Ensures current GPT Codex model ids parse as Codex models.
fn test_parse_model_parses_current_codex_models() {
    // Arrange
    let codex_kind = AgentKind::Codex;

    // Act
    let parsed_astra = codex_kind.parse_model("gpt-6-astra");
    let parsed_sol = codex_kind.parse_model("gpt-5.6-sol");
    let parsed_terra = codex_kind.parse_model("gpt-5.6-terra");
    let parsed_luna = codex_kind.parse_model("gpt-5.6-luna");
    let parsed_spark = codex_kind.parse_model("gpt-5.3-codex-spark");

    // Assert
    assert_eq!(parsed_astra, Some(AgentModel::Gpt6Astra));
    assert_eq!(parsed_sol, Some(AgentModel::Gpt56Sol));
    assert_eq!(parsed_terra, Some(AgentModel::Gpt56Terra));
    assert_eq!(parsed_luna, Some(AgentModel::Gpt56Luna));
    assert_eq!(parsed_spark, Some(AgentModel::Gpt53CodexSpark));
}

#[test]
/// Ensures Antigravity parses raw Gemini model ids as shared Gemini model
/// selections.
fn test_parse_model_parses_antigravity() {
    // Arrange
    let antigravity_kind = AgentKind::Antigravity;

    // Act
    let parsed_pro = antigravity_kind.parse_model("gemini-3.1-pro-preview");
    let parsed_flash_38 = antigravity_kind.parse_model("gemini-3.8-flash");
    let parsed_flash_35_lite = antigravity_kind.parse_model("gemini-3.5-flash-lite");

    // Assert
    assert_eq!(parsed_pro, Some(AgentModel::Gemini31Pro));
    assert_eq!(parsed_flash_38, Some(AgentModel::Gemini38Flash));
    assert_eq!(parsed_flash_35_lite, Some(AgentModel::Gemini35FlashLite));
}

#[test]
/// Ensures direct Gemini parses the same raw Gemini model ids as
/// Antigravity.
fn test_parse_model_parses_direct_gemini() {
    // Arrange
    let gemini_kind = AgentKind::Gemini;

    // Act
    let parsed_model = gemini_kind.parse_model("gemini-3.8-flash");

    // Assert
    assert_eq!(parsed_model, Some(AgentModel::Gemini38Flash));
}

#[test]
/// Ensures both Google providers reject the invalid non-preview Gemini
/// 3.1 Pro identifier.
fn test_parse_model_rejects_non_preview_gemini_31_pro() {
    // Arrange
    let agent_kinds = [AgentKind::Gemini, AgentKind::Antigravity];

    // Act
    let parsed_models = agent_kinds.map(|kind| kind.parse_model("gemini-3.1-pro"));

    // Assert
    assert_eq!(parsed_models, [None; 2]);
}

#[test]
/// Ensures current Gemini models expose their user-facing descriptions.
fn test_current_gemini_models_have_current_descriptions() {
    // Arrange
    let models = [
        AgentModel::Gemini31Pro,
        AgentModel::Gemini38Flash,
        AgentModel::Gemini35FlashLite,
    ];

    // Act
    let descriptions = models.map(|model| model.description());

    // Assert
    assert_eq!(
        descriptions,
        [
            "Higher-quality Gemini model for deeper reasoning.",
            "Fast Gemini model for agentic and multimodal tasks.",
            "Lightweight Gemini model for fast, cost-conscious workloads.",
        ]
    );
}

#[test]
/// Ensures retired GPT model ids no longer parse as selectable models.
fn test_parse_model_rejects_retired_gpt_models() {
    // Arrange
    let codex_kind = AgentKind::Codex;

    // Act
    let parsed_gpt_54 = codex_kind.parse_model("gpt-5.4");
    let parsed_gpt_54_mini = codex_kind.parse_model("gpt-5.4-mini");

    // Assert
    assert_eq!(parsed_gpt_54, None);
    assert_eq!(parsed_gpt_54_mini, None);
}

#[test]
/// Ensures retired Claude aliases no longer parse as selectable
/// models.
fn test_parse_model_rejects_retired_claude_aliases() {
    // Arrange
    let claude_kind = AgentKind::Claude;

    // Act
    let parsed_opus_46 = claude_kind.parse_model("claude-opus-4-6");
    let parsed_opus_47 = claude_kind.parse_model("claude-opus-4-7");
    let parsed_sonnet_46 = claude_kind.parse_model("claude-sonnet-4-6");

    // Assert
    assert_eq!(parsed_opus_46, None);
    assert_eq!(parsed_opus_47, None);
    assert_eq!(parsed_sonnet_46, None);
}

#[test]
/// Ensures current Claude model ids parse as supported Claude models.
fn test_parse_model_parses_current_claude_models() {
    // Arrange
    let claude_kind = AgentKind::Claude;

    // Act
    let parsed_opus_5 = claude_kind.parse_model("claude-opus-5");
    let parsed_sonnet_5 = claude_kind.parse_model("claude-sonnet-5");
    let parsed_fable_5 = claude_kind.parse_model("claude-fable-5");
    let parsed_haiku_45 = claude_kind.parse_model("claude-haiku-4-5-20251001");
    let opus_5_id = AgentModel::ClaudeOpus5.as_str();
    let opus_5_description = AgentModel::ClaudeOpus5.description();

    // Assert
    assert_eq!(parsed_opus_5, Some(AgentModel::ClaudeOpus5));
    assert_eq!(parsed_sonnet_5, Some(AgentModel::ClaudeSonnet5));
    assert_eq!(parsed_fable_5, Some(AgentModel::ClaudeFable5));
    assert_eq!(parsed_haiku_45, Some(AgentModel::ClaudeHaiku4520251001));
    assert_eq!(opus_5_id, "claude-opus-5");
    assert_eq!(
        opus_5_description,
        "Latest Claude Opus model for complex tasks."
    );
}

#[test]
/// Ensures Codex models are supported by Codex only.
fn test_codex_models_are_supported_by_codex() {
    // Arrange
    let models = [
        AgentModel::Gpt6Astra,
        AgentModel::Gpt56Sol,
        AgentModel::Gpt56Terra,
        AgentModel::Gpt56Luna,
        AgentModel::Gpt53CodexSpark,
    ];

    // Act
    let supported = models.map(|model| AgentKind::Codex.supports_model(model));
    let unsupported = models.map(|model| AgentKind::Claude.supports_model(model));

    // Assert
    assert_eq!(supported, [true; 5]);
    assert_eq!(unsupported, [false; 5]);
}

#[test]
/// Ensures Claude models are supported by Claude only.
fn test_claude_models_are_supported_by_claude() {
    // Arrange
    let models = [
        AgentModel::ClaudeOpus5,
        AgentModel::ClaudeSonnet5,
        AgentModel::ClaudeFable5,
        AgentModel::ClaudeHaiku4520251001,
    ];

    // Act
    let supported = models.map(|model| AgentKind::Claude.supports_model(model));
    let unsupported = models.map(|model| AgentKind::Codex.supports_model(model));

    // Assert
    assert_eq!(supported, [true; 4]);
    assert_eq!(unsupported, [false; 4]);
}

#[test]
/// Ensures shared Gemini models are supported by both Google providers.
fn test_gemini_models_are_supported_by_gemini_and_antigravity() {
    // Arrange
    let models = [
        AgentModel::Gemini31Pro,
        AgentModel::Gemini38Flash,
        AgentModel::Gemini35FlashLite,
    ];

    // Act
    let antigravity_supported = models.map(|model| AgentKind::Antigravity.supports_model(model));
    let gemini_supported = models.map(|model| AgentKind::Gemini.supports_model(model));
    let codex_supported = models.map(|model| AgentKind::Codex.supports_model(model));

    // Assert
    assert_eq!(antigravity_supported, [true; 3]);
    assert_eq!(gemini_supported, [true; 3]);
    assert_eq!(codex_supported, [false; 3]);
}

#[test]
/// Ensures only providers with a response-speed control report support.
fn test_supports_speed_mode_covers_claude_and_codex_only() {
    // Arrange
    let kinds = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::Antigravity,
    ];

    // Act
    let supported = kinds.map(AgentKind::supports_speed_mode);

    // Assert
    assert_eq!(supported, [true, true, false, false]);
}

#[test]
/// Ensures Antigravity model selections pass raw Gemini ids to provider
/// transports.
fn test_antigravity_provider_model_str_returns_raw_gemini_model() {
    // Arrange
    let model = AgentModel::Gemini35FlashLite;

    // Act
    let persisted_model = model.as_str();
    let provider_model = model.provider_model_str();

    // Assert
    assert_eq!(persisted_model, "gemini-3.5-flash-lite");
    assert_eq!(provider_model, "gemini-3.5-flash-lite");
}
