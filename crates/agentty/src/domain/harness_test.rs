use crate::domain::agent::{
    AgentModel, HarnessModelOption, HarnessModelStatus, ModelBilling, ServedModel,
};
use crate::domain::harness::{HarnessAvailability, HarnessModelCatalog};

/// Returns one hosted option with `status`.
fn kimi(status: HarnessModelStatus) -> HarnessModelOption {
    HarnessModelOption {
        billing: ModelBilling::Paid,
        context_window: None,
        loaded_context_window: None,
        model: AgentModel::KimiK3,
        provider: "Kimi".to_string(),
        status,
    }
}

/// Returns one ready local option.
fn local_qwen() -> HarnessModelOption {
    HarnessModelOption {
        billing: ModelBilling::Local,
        model: AgentModel::OpenAiCompatible(ServedModel::new("qwen3:latest").expect("name")),
        provider: "Ollama".to_string(),
        ..kimi(HarnessModelStatus::Ready)
    }
}

#[test]
/// Hides the row without the flag, disables it without a runnable model,
/// and otherwise starts on the catalog default.
fn harness_availability_follows_flag_and_runnable_models() {
    // Arrange
    let missing_key = kimi(HarnessModelStatus::MissingConfiguration("KIMI_API_KEY"));
    let cases = [
        (
            HarnessModelCatalog::new(false, vec![kimi(HarnessModelStatus::Ready)], None),
            HarnessAvailability::Hidden,
            false,
        ),
        (
            HarnessModelCatalog::new(true, vec![missing_key], None),
            HarnessAvailability::MissingCredentials,
            true,
        ),
        (
            HarnessModelCatalog::new(true, vec![kimi(HarnessModelStatus::Ready)], None),
            HarnessAvailability::Available(AgentModel::KimiK3),
            true,
        ),
    ];

    // Act / Assert
    for (catalog, expected, visible) in cases {
        let availability = catalog.availability();
        assert_eq!(availability, expected);
        assert_eq!(availability.is_visible(), visible);
        assert_eq!(catalog.is_enabled(), visible);
        assert_eq!(
            availability.is_available(),
            availability.default_model().is_some()
        );
    }
    assert_eq!(HarnessAvailability::default(), HarnessAvailability::Hidden);
}

#[test]
/// Starts new sessions on a free local model, then on the remembered choice
/// once the user picks one, and keeps refreshed options.
fn catalog_prefers_local_models_until_a_choice_is_remembered() {
    // Arrange
    let mut catalog = HarnessModelCatalog::new(true, Vec::new(), None);
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
