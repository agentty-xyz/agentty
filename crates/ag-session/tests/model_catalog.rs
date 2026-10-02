//! Public model catalog, compatibility, and token-budget contracts.

use std::collections::HashSet;

use ag_session::{AgentKind, AgentModel, AgentSelectionMetadata, ModelContextLimits};

#[test]
fn provider_catalog_preserves_wire_ids_order_and_defaults() {
    // Arrange
    let cases = [
        (
            AgentKind::Gemini,
            vec![
                "gemini-3.1-pro-preview",
                "gemini-3.8-flash",
                "gemini-3.5-flash-lite",
            ],
            "gemini-3.1-pro-preview",
        ),
        (
            AgentKind::Antigravity,
            vec![
                "gemini-3.1-pro-preview",
                "gemini-3.8-flash",
                "gemini-3.5-flash-lite",
            ],
            "gemini-3.1-pro-preview",
        ),
        (
            AgentKind::Claude,
            vec![
                "claude-fable-5-1",
                "claude-opus-5-5",
                "claude-sonnet-5",
                "claude-haiku-4-5-20251001",
            ],
            "claude-fable-5-1",
        ),
        (
            AgentKind::Codex,
            vec![
                "gpt-6-astra",
                "gpt-6.1-sol",
                "gpt-6-luna",
                "gpt-5.6-terra",
                "gpt-5.3-codex-spark",
            ],
            "gpt-6.1-sol",
        ),
    ];

    // Act / Assert
    for (provider, expected_ids, expected_default) in cases {
        let actual_ids: Vec<_> = provider
            .models()
            .iter()
            .map(|model| model.as_str())
            .collect();
        assert_eq!(actual_ids, expected_ids);
        assert_eq!(provider.default_model().as_str(), expected_default);
        assert!(provider.supports_model(provider.default_model()));
        for model in provider.models() {
            assert_eq!(provider.parse_model(model.as_str()), Some(*model));
        }
    }
}

#[test]
fn every_catalog_model_has_unique_roundtrippable_metadata() {
    // Arrange
    let models = AgentModel::ALL;

    // Act
    let ids: HashSet<_> = models.iter().map(|model| model.as_str()).collect();

    // Assert
    assert_eq!(ids.len(), models.len());
    for model in models {
        assert_eq!(model.as_str().parse::<AgentModel>(), Ok(*model));
        assert_eq!(model.name(), model.as_str());
        assert_ne!(model.description(), "");
        assert!(
            AgentKind::ALL
                .iter()
                .any(|provider| provider.supports_model(*model))
        );
    }
    assert_eq!(
        "unknown-model".parse::<AgentModel>(),
        Err("unknown model: unknown-model".to_string())
    );
    assert!("gpt-6-sol".parse::<AgentModel>().is_err());
    assert_eq!(
        AgentModel::parse_persisted("gpt-6-sol"),
        Ok(AgentModel::Gpt61Sol)
    );
    assert!("claude-fable-5".parse::<AgentModel>().is_err());
    assert_eq!(
        AgentModel::parse_persisted("claude-fable-5"),
        Ok(AgentModel::ClaudeFable51)
    );
}

#[test]
fn declared_context_limits_preserve_proactive_compaction_budgets() {
    // Arrange
    let codex_models = AgentKind::Codex.models();

    // Act / Assert
    for model in codex_models {
        let expected = if *model == AgentModel::Gpt53CodexSpark {
            (128_000, 8_000, 120_000)
        } else {
            (1_050_000, 128_000, 922_000)
        };
        let limits = model
            .context_limits()
            .expect("Codex models declare compaction limits");
        assert_eq!(limits.context_window_tokens, expected.0);
        assert_eq!(limits.input_headroom_tokens, expected.1);
        assert_eq!(limits.input_token_budget(), expected.2);
    }
    for provider in [AgentKind::Gemini, AgentKind::Antigravity, AgentKind::Claude] {
        assert!(
            provider
                .models()
                .iter()
                .all(|model| model.context_limits().is_none())
        );
    }
}

#[test]
fn input_budget_saturates_when_headroom_exceeds_capacity() {
    // Arrange
    let limits = ModelContextLimits {
        context_window_tokens: 100,
        input_headroom_tokens: 101,
    };

    // Act
    let budget = limits.input_token_budget();

    // Assert
    assert_eq!(budget, 0);
}
