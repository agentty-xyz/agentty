use std::env;

use ag_harness::provider::ModelProvider;
use serde_json::json;

use super::support::FixedModel;
use crate::{ChatCommand, CliError, ModelSelection, model_connector};

fn muse_selection(model: &str) -> ModelSelection {
    ModelSelection {
        model: model.to_string(),
        provider: ModelProvider::Muse,
    }
}

#[test]
fn chat_commands_are_parsed_from_slash_words_but_not_paths() {
    // Arrange and Act
    let help = ChatCommand::parse("/");
    let named_help = ChatCommand::parse(" /help please ");
    let list = ChatCommand::parse("/model");
    let switch = ChatCommand::parse("/model   kimi/kimi-test  ");
    let unknown = ChatCommand::parse("/models now");
    let path = ChatCommand::parse("/usr/bin/env explain this");
    let prompt = ChatCommand::parse("hello /model");

    // Assert
    assert_eq!(help, Some(ChatCommand::Help));
    assert_eq!(named_help, Some(ChatCommand::Help));
    assert_eq!(list, Some(ChatCommand::ListModels));
    assert_eq!(
        switch,
        Some(ChatCommand::SwitchModel("kimi/kimi-test".to_string()))
    );
    assert_eq!(unknown, Some(ChatCommand::Unknown("/models".to_string())));
    assert_eq!(path, None);
    assert_eq!(prompt, None);
}

#[test]
fn model_selection_resolves_numbers_providers_and_known_models() {
    // Arrange
    let current = muse_selection("muse-test");
    let catalog = ModelSelection::catalog();
    let kimi_model = ModelProvider::Kimi.known_models()[0];

    // Act
    let numbered = ModelSelection::parse("1", &current);
    let qualified = ModelSelection::parse("qwen/qwen-custom", &current);
    let known = ModelSelection::parse(kimi_model, &current);
    let custom = ModelSelection::parse("muse-custom", &current);
    let zero = ModelSelection::parse("0", &current);
    let out_of_range = ModelSelection::parse(&(catalog.len() + 1).to_string(), &current);
    let unknown_provider = ModelSelection::parse("unknown/model", &current);

    // Assert
    assert_eq!(numbered.expect("first catalog model"), catalog[0]);
    assert_eq!(
        qualified.expect("qualified model").key(),
        "qwen/qwen-custom"
    );
    assert_eq!(
        known.expect("known model").key(),
        format!("kimi/{kimi_model}")
    );
    assert_eq!(custom.expect("custom model").key(), "muse/muse-custom");
    assert!(matches!(zero, Err(CliError::UnknownModel { model }) if model == "0"));
    assert!(matches!(out_of_range, Err(CliError::UnknownModel { .. })));
    assert!(matches!(
        unknown_provider,
        Err(CliError::UnknownModel { model }) if model == "unknown/model"
    ));
}

#[test]
fn model_catalog_lists_every_known_provider_model() {
    // Arrange and Act
    let catalog = ModelSelection::catalog();

    // Assert
    let expected = ModelProvider::all()
        .iter()
        .flat_map(|provider| {
            provider
                .known_models()
                .iter()
                .map(move |model| format!("{provider}/{model}"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        catalog.iter().map(ModelSelection::key).collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn model_connector_applies_an_explicit_base_url_only_to_its_provider() {
    // Arrange
    let mut environment = |name: &str| {
        if name.ends_with("_KEY") {
            Ok("test-key".to_string())
        } else {
            Err(env::VarError::NotPresent)
        }
    };
    let mut connect = model_connector(
        ModelProvider::Kimi,
        Some("https://models.example/v1".to_string()),
        &mut environment,
    );

    // Act
    let kimi = connect(&ModelSelection {
        model: "kimi-test".to_string(),
        provider: ModelProvider::Kimi,
    });
    let qwen = connect(&ModelSelection {
        model: "qwen-test".to_string(),
        provider: ModelProvider::Qwen,
    });

    // Assert
    assert!(kimi.is_ok());
    assert!(matches!(
        qwen,
        Err(CliError::BaseUrlRequired { name }) if name == ModelProvider::Qwen.base_url_environment()
    ));
}

#[test]
fn model_identity_is_absent_beyond_the_identity_limit() {
    // Arrange
    let longest = muse_selection(&"m".repeat(256 - "muse/".len()));
    let oversized = muse_selection(&"m".repeat(256 - "muse/".len() + 1));

    // Act
    let identity = longest.identity();
    let missing = oversized.identity();

    // Assert
    let identity = identity.expect("a 256-byte key should be registrable");
    assert_eq!(identity.key(), longest.key());
    assert!(ModelSelection::registry(identity, FixedModel(json!({}))).is_ok());
    assert_eq!(missing, None);
}
