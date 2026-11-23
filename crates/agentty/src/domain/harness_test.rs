//! Harness availability and model catalog preference tests.

use crate::domain::agent::{
    AgentModel, HarnessModelOption, HarnessModelStatus, ModelCost, ServedModel,
};
use crate::domain::harness::{HarnessAvailability, HarnessModelCatalog};

/// Returns one hosted option with `status`.
fn kimi(status: HarnessModelStatus) -> HarnessModelOption {
    HarnessModelOption {
        context_window: None,
        cost: ModelCost::Premium,
        loaded_context_window: None,
        model: AgentModel::KimiK3,
        provider: "Kimi".to_string(),
        status,
        trains_on_prompts: false,
    }
}

/// Returns one ready local option.
fn local_qwen() -> HarnessModelOption {
    HarnessModelOption {
        cost: ModelCost::Local,
        model: AgentModel::OpenAiCompatible(ServedModel::new("qwen3:latest").expect("name")),
        provider: "Ollama".to_string(),
        ..kimi(HarnessModelStatus::Ready)
    }
}

#[test]
/// Offers no Harness sessions without a runnable model, and otherwise starts
/// them on the catalog default.
fn harness_availability_follows_runnable_models() {
    // Arrange
    let missing_key = kimi(HarnessModelStatus::MissingConfiguration("KIMI_API_KEY"));
    let cases = [
        (
            HarnessModelCatalog::default(),
            HarnessAvailability::MissingCredentials,
        ),
        (
            HarnessModelCatalog::new(vec![missing_key], None),
            HarnessAvailability::MissingCredentials,
        ),
        (
            HarnessModelCatalog::new(vec![kimi(HarnessModelStatus::Ready)], None),
            HarnessAvailability::Available(AgentModel::KimiK3),
        ),
    ];

    // Act / Assert
    for (catalog, expected) in cases {
        let availability = catalog.availability();
        assert_eq!(availability, expected);
        assert_eq!(
            availability.is_available(),
            availability.default_model().is_some()
        );
    }
    assert_eq!(
        HarnessAvailability::default(),
        HarnessAvailability::MissingCredentials
    );
}

#[test]
/// Starts new sessions on a free local model, then on the remembered choice
/// once the user picks one, and keeps refreshed options.
fn catalog_prefers_local_models_until_a_choice_is_remembered() {
    // Arrange
    let mut catalog = HarnessModelCatalog::new(Vec::new(), None);
    catalog.replace_options(vec![kimi(HarnessModelStatus::Ready), local_qwen()]);
    let local_default = catalog.availability();

    // Act
    catalog.remember(AgentModel::KimiK3);

    // Assert
    assert_eq!(local_default.default_model(), Some(local_qwen().model));
    assert_eq!(
        catalog.availability(),
        HarnessAvailability::Available(AgentModel::KimiK3)
    );
    assert_eq!(catalog.options().len(), 2);
}
