use crate::agent::{AgentModel, ServedModel};
use crate::harness_model::{
    HarnessModelOption, HarnessModelStatus, ModelCost, default_harness_model,
};

/// Returns one served local model option.
fn local(name: &str, status: HarnessModelStatus) -> HarnessModelOption {
    HarnessModelOption {
        context_window: Some(40_960),
        cost: ModelCost::Local,
        loaded_context_window: None,
        model: served(name),
        provider: "Ollama".to_string(),
        status,
        trains_on_prompts: false,
    }
}

/// Returns one hosted paid model option.
fn paid(model: AgentModel, status: HarnessModelStatus) -> HarnessModelOption {
    HarnessModelOption {
        context_window: None,
        cost: ModelCost::Standard,
        loaded_context_window: None,
        model,
        provider: "Kimi".to_string(),
        status,
        trains_on_prompts: false,
    }
}

/// Returns the served model `name`.
fn served(name: &str) -> AgentModel {
    AgentModel::OpenAiCompatible(ServedModel::new(name).expect("served name"))
}

#[test]
fn default_prefers_a_ready_local_model_over_a_ready_paid_model() {
    // Arrange
    let options = [
        paid(AgentModel::KimiK3, HarnessModelStatus::Ready),
        local("gemma:latest", HarnessModelStatus::NoToolSupport),
        local("qwen3:latest", HarnessModelStatus::Ready),
    ];

    // Act
    let model = default_harness_model(&options, None);

    // Assert
    assert_eq!(model, Some(served("qwen3:latest")));
}

#[test]
fn default_falls_back_to_a_ready_paid_model_without_local_models() {
    // Arrange
    let options = [
        paid(
            AgentModel::MuseSpark13,
            HarnessModelStatus::MissingConfiguration("MODEL_API_KEY"),
        ),
        paid(AgentModel::KimiK3, HarnessModelStatus::Ready),
    ];

    // Act
    let model = default_harness_model(&options, None);

    // Assert
    assert_eq!(model, Some(AgentModel::KimiK3));
}

#[test]
fn default_keeps_the_remembered_ready_paid_model() {
    // Arrange
    let options = [
        local("qwen3:latest", HarnessModelStatus::Ready),
        paid(AgentModel::KimiK3, HarnessModelStatus::Ready),
    ];

    // Act
    let model = default_harness_model(&options, Some(AgentModel::KimiK3));

    // Assert
    assert_eq!(model, Some(AgentModel::KimiK3));
}

#[test]
fn default_keeps_a_remembered_local_model_while_its_server_is_unreachable() {
    // Arrange
    let options = [paid(AgentModel::KimiK3, HarnessModelStatus::Ready)];

    // Act
    let model = default_harness_model(&options, Some(served("qwen3:latest")));

    // Assert
    assert_eq!(model, Some(served("qwen3:latest")));
}

#[test]
fn default_replaces_a_remembered_paid_model_whose_key_is_gone() {
    // Arrange
    let options = [
        paid(
            AgentModel::KimiK3,
            HarnessModelStatus::MissingConfiguration("KIMI_API_KEY"),
        ),
        local("qwen3:latest", HarnessModelStatus::Ready),
    ];

    // Act
    let model = default_harness_model(&options, Some(AgentModel::KimiK3));

    // Assert
    assert_eq!(model, Some(served("qwen3:latest")));
}

#[test]
fn default_is_none_when_no_model_is_ready() {
    // Arrange
    let options = [paid(
        AgentModel::KimiK3,
        HarnessModelStatus::MissingConfiguration("KIMI_API_KEY"),
    )];

    // Act
    let model = default_harness_model(&options, None);

    // Assert
    assert_eq!(model, None);
}

#[test]
fn default_keeps_catalog_order_over_a_cheaper_ready_paid_model() {
    // Arrange
    let cheap = HarnessModelOption {
        cost: ModelCost::Cheap,
        ..paid(AgentModel::Qwen38Flash, HarnessModelStatus::Ready)
    };
    let options = [
        paid(AgentModel::MuseSpark13, HarnessModelStatus::Ready),
        cheap,
    ];

    // Act
    let model = default_harness_model(&options, None);

    // Assert
    assert_eq!(model, Some(AgentModel::MuseSpark13));
}

#[test]
fn model_cost_labels_free_local_models_and_paid_price_classes() {
    // Arrange
    let costs = [
        ModelCost::Local,
        ModelCost::Cheap,
        ModelCost::Standard,
        ModelCost::Premium,
    ];

    // Act
    let labels = costs.map(|cost| (cost.label(), cost.is_paid()));

    // Assert
    assert_eq!(
        labels,
        [
            ("free", false),
            ("cheap", true),
            ("standard", true),
            ("premium", true)
        ]
    );
}
