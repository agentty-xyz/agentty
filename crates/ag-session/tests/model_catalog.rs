//! Public model catalog and compatibility contracts.

use std::collections::HashSet;

use ag_session::{AgentKind, AgentModel, AgentSelectionMetadata};

#[test]
fn provider_catalog_preserves_wire_ids_order_and_defaults() {
    // Arrange
    let cases = [
        (
            AgentKind::Gemini,
            vec![
                "gemini-4-argon",
                "gemini-3.1-pro-preview",
                "gemini-3.8-flash",
                "gemini-3.5-flash-lite",
            ],
            "gemini-3.1-pro-preview",
        ),
        (
            AgentKind::Antigravity,
            vec![
                "gemini-4-argon",
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
                "claude-sonnet-5-5",
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
        (
            AgentKind::Harness,
            vec![
                "muse-spark-1.3",
                "kimi-k3",
                "kimi-k2.6",
                "qwen3.8-max",
                "qwen-plus",
                "gpt-6.1-sol",
                "gpt-6-astra",
                "gpt-6-luna",
                "gpt-5.6-terra",
            ],
            "muse-spark-1.3",
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
        assert_eq!(model.provider_model_str(), model.as_str());
        assert_eq!(AgentModel::parse_persisted(model.as_str()), Ok(*model));
        assert_ne!(model.description(), "");
        assert!(
            AgentKind::ALL
                .iter()
                .chain([&AgentKind::Harness])
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
    assert!("claude-sonnet-5".parse::<AgentModel>().is_err());
    assert_eq!(
        AgentModel::parse_persisted("claude-sonnet-5"),
        Ok(AgentModel::ClaudeSonnet55)
    );
    assert!("claude-fable-5".parse::<AgentModel>().is_err());
    assert_eq!(
        AgentModel::parse_persisted("claude-fable-5"),
        Ok(AgentModel::ClaudeFable51)
    );
}
