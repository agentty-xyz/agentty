use ag_harness::OutputSchema;
use ag_harness::model::{ContextEstimator, HeuristicContextEstimator};
use ag_session::{AgentKind, AgentModel, ServedModel};
use serde_json::json;

use crate::agent::model_profile::declared;

#[test]
fn declared_profiles_preserve_proactive_compaction_budgets() {
    // Arrange
    let codex_models = AgentKind::Codex.models();
    let harness_models = [
        (AgentModel::MuseSpark13, (1_048_576, 230_107, 818_469)),
        (
            AgentModel::MuseSpark13Contributor,
            (1_048_576, 230_107, 818_469),
        ),
        (AgentModel::KimiK3, (1_048_576, 223_112, 825_464)),
        (AgentModel::KimiK26, (262_144, 55_778, 206_366)),
        (AgentModel::Qwen38Max, (1_000_000, 204_643, 795_357)),
        (AgentModel::QwenPlus, (1_000_000, 122_295, 877_705)),
    ];

    // Act / Assert
    for model in codex_models {
        let expected = if *model == AgentModel::Gpt53CodexSpark {
            (128_000, 8_000, 120_000)
        } else {
            (1_050_000, 128_000, 922_000)
        };
        let profile = declared(*model).expect("Codex models declare compaction profiles");
        assert_eq!(profile.context_window_tokens(), expected.0, "{model:?}");
        assert_eq!(profile.reserved_tokens(), expected.1, "{model:?}");
        assert_eq!(profile.input_token_budget(), expected.2, "{model:?}");
    }
    for (model, expected) in harness_models {
        let profile = declared(model).expect("harness models declare context profiles");
        assert_eq!(profile.context_window_tokens(), expected.0, "{model:?}");
        assert_eq!(profile.reserved_tokens(), expected.1, "{model:?}");
        assert_eq!(profile.input_token_budget(), expected.2, "{model:?}");
    }
    // The harness catalog declares the Qwen flash profiles itself.
    let catalog_profiled = [AgentModel::Qwen38Flash, AgentModel::Qwen37Flash];
    assert_eq!(
        AgentKind::Harness.models().len(),
        harness_models.len() + catalog_profiled.len() + codex_models.len() - 1
    );
    assert!(
        catalog_profiled
            .iter()
            .all(|model| declared(*model).is_none())
    );
    assert_eq!(
        declared(AgentModel::OpenAiCompatible(ServedModel::CONFIGURED)),
        None
    );
    for provider in [AgentKind::Gemini, AgentKind::Antigravity, AgentKind::Claude] {
        assert!(
            provider
                .models()
                .iter()
                .all(|model| declared(*model).is_none())
        );
    }
}

#[test]
fn harness_models_weigh_requests_with_measured_estimators() {
    // Arrange
    // The harness profiles the shared Codex models and the Qwen flash models
    // from its provider catalog, and served OpenAI-compatible models from the
    // server or environment.
    let harness_models = AgentKind::Harness
        .models()
        .iter()
        .filter(|model| !AgentKind::Codex.supports_model(**model))
        .filter(|model| !matches!(model, AgentModel::Qwen38Flash | AgentModel::Qwen37Flash))
        .collect::<Vec<_>>();

    // Act
    let estimators = harness_models
        .iter()
        .map(|model| {
            declared(**model)
                .expect("harness models declare context profiles")
                .estimator()
        })
        .collect::<Vec<_>>();

    // Assert
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    for (model, estimator) in harness_models.iter().zip(&estimators) {
        assert_ne!(*estimator, HeuristicContextEstimator::DEFAULT, "{model:?}");
        // Every measured provider renders the output schema into the prompt.
        assert!(estimator.request_weight(&schema) > 0, "{model:?}");
    }
    for model in AgentKind::Codex.models() {
        let profile = declared(*model).expect("Codex models declare compaction profiles");
        assert_eq!(profile.estimator(), HeuristicContextEstimator::DEFAULT);
    }
}
