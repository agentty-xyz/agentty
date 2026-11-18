//! Chat command parsing, model selection from the catalog, model listing,
//! and model connector configuration unit tests.

use std::env;
use std::sync::Arc;

use ag_harness::catalog::{ModelCatalog, Role};
use ag_harness::model::ModelProfile;
use ag_harness::provider::{GPT_6_LUNA, ModelProvider};
use serde_json::json;

use super::support::{FixedModel, test_catalog};
use crate::{
    ChatCommand, CliError, MODEL_PROFILE, ModelSelection, ModelSwitcher, model_catalog,
    model_connector,
};

fn muse_selection(model: &str) -> ModelSelection {
    ModelSelection {
        model: model.to_string(),
        provider: ModelProvider::Muse,
    }
}

/// Switcher over `catalog` whose connected clients answer with an empty
/// object.
fn switcher(
    catalog: ModelCatalog,
    selection: ModelSelection,
) -> ModelSwitcher<impl FnMut(&ModelSelection) -> Result<(FixedModel, ModelProfile), CliError>> {
    ModelSwitcher {
        catalog: Arc::new(catalog),
        connect: |_: &ModelSelection| Ok((FixedModel(json!({})), MODEL_PROFILE)),
        selection,
    }
}

#[test]
fn chat_commands_are_parsed_from_slash_words_but_not_paths() {
    // Arrange and Act
    let help = ChatCommand::parse("/");
    let named_help = ChatCommand::parse(" /help please ");
    let list = ChatCommand::parse("/model");
    let switch = ChatCommand::parse("/model   kimi/kimi-test  ");
    let usage = ChatCommand::parse("/usage");
    let history = ChatCommand::parse("/history");
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
    assert_eq!(usage, Some(ChatCommand::Usage));
    assert_eq!(history, Some(ChatCommand::History));
    assert_eq!(unknown, Some(ChatCommand::Unknown("/models".to_string())));
    assert_eq!(path, None);
    assert_eq!(prompt, None);
}

#[test]
fn model_selection_resolves_numbers_roles_providers_and_known_models() {
    // Arrange
    let current = muse_selection("muse-test");
    let catalog = test_catalog();
    let kimi_model = ModelProvider::Kimi.known_models()[0];

    // Act
    let numbered = ModelSelection::parse("2", &current, &catalog);
    let host = ModelSelection::parse("1", &current, &catalog);
    let role = ModelSelection::parse("cheap", &current, &catalog);
    let qualified = ModelSelection::parse("qwen/qwen-custom", &current, &catalog);
    let known = ModelSelection::parse(kimi_model, &current, &catalog);
    let custom = ModelSelection::parse("muse-custom", &current, &catalog);
    let unassigned_role = ModelSelection::parse("smart", &current, &catalog);
    let role_named_model = ModelSelection::parse("openai-compatible/fast", &current, &catalog);
    let zero = ModelSelection::parse("0", &current, &catalog);
    let out_of_range = ModelSelection::parse("4", &current, &catalog);
    let unknown_provider = ModelSelection::parse("unknown/model", &current, &catalog);

    // Assert
    assert_eq!(
        numbered.expect("listed model").id().as_str(),
        "kimi/kimi-listed"
    );
    assert!(matches!(host, Err(CliError::UnknownModel { model }) if model == "1"));
    assert_eq!(role.expect("role model").id().as_str(), "kimi/kimi-listed");
    assert_eq!(
        qualified.expect("qualified model").id().as_str(),
        "qwen/qwen-custom"
    );
    assert_eq!(
        known.expect("known model").id().to_string(),
        format!("kimi/{kimi_model}")
    );
    assert_eq!(
        custom.expect("custom model").id().as_str(),
        "muse/muse-custom"
    );
    assert!(matches!(
        &unassigned_role,
        Err(error @ CliError::UnassignedRole { role }) if *role == Role::SMART
            && error.to_string()
                == "no configured model serves the `smart` role; type /model to list models"
    ));
    assert_eq!(
        role_named_model.expect("qualified model").id().as_str(),
        "openai-compatible/fast"
    );
    assert!(matches!(zero, Err(CliError::UnknownModel { model }) if model == "0"));
    assert!(matches!(out_of_range, Err(CliError::UnknownModel { .. })));
    assert!(matches!(
        unknown_provider,
        Err(CliError::UnknownModel { model }) if model == "unknown/model"
    ));
}

#[test]
fn bare_known_model_ids_prefer_the_current_provider_then_provider_order() {
    // Arrange
    let catalog = test_catalog();
    let chatgpt = ModelSelection {
        model: "any".to_string(),
        provider: ModelProvider::ChatGpt,
    };

    // Act
    let from_muse = ModelSelection::parse(GPT_6_LUNA, &muse_selection("muse-test"), &catalog);
    let from_chatgpt = ModelSelection::parse(GPT_6_LUNA, &chatgpt, &catalog);

    // Assert
    assert_eq!(
        from_muse.expect("known model").provider,
        ModelProvider::OpenAi
    );
    assert_eq!(
        from_chatgpt.expect("known model").provider,
        ModelProvider::ChatGpt
    );
}

#[test]
fn model_list_numbers_catalog_models_with_roles_and_prices() {
    // Arrange
    let models = switcher(test_catalog(), muse_selection("muse-test"));
    let empty = switcher(
        ModelCatalog::builder().build().expect("empty catalog"),
        muse_selection("muse-test"),
    );

    // Act
    let listed = models.list();
    let unlisted = empty.list();

    // Assert
    assert_eq!(
        listed,
        "current model: muse/muse-test\n  1. host/model\n  2. kimi/kimi-listed (cheap) $0.60 in, \
         $2.50 out per 1M tokens\n  3. qwen/qwen-listed\nType /model <MODEL> to switch.\n"
    );
    assert_eq!(
        unlisted,
        "current model: muse/muse-test\n  no model is cataloged; set a provider's credentials to \
         list its models\nType /model <MODEL> to switch.\n"
    );
}

#[test]
fn model_catalog_applies_an_explicit_base_url_only_to_its_provider() {
    // Arrange
    let mut environment = |name: &str| match name {
        "KIMI_API_KEY" | "DASHSCOPE_API_KEY" => Ok("test-key".to_string()),
        _ => Err(env::VarError::NotPresent),
    };
    let kimi = ModelProvider::Kimi.known_models()[0];

    let mut warnings = Vec::new();

    // Act
    let with_kimi_url = model_catalog(
        ModelProvider::Kimi,
        Some("https://models.example/v1"),
        &mut environment,
        &mut warnings,
    )
    .expect("Kimi with a base URL should be cataloged");
    let with_qwen_url = model_catalog(
        ModelProvider::Qwen,
        Some("https://models.example/v1"),
        &mut environment,
        &mut warnings,
    )
    .expect("Kimi without a base URL should be skipped");

    // Assert
    let kimi_id = ModelSelection {
        model: kimi.to_string(),
        provider: ModelProvider::Kimi,
    }
    .id();
    assert!(with_kimi_url.spec(&kimi_id).is_some());
    assert!(with_qwen_url.spec(&kimi_id).is_none());
    assert!(with_qwen_url.specs().next().is_some());
    assert_eq!(warnings, [] as [u8; 0]);
}

#[test]
fn an_unusable_provider_leaves_the_catalog_empty_with_a_warning() {
    // Arrange
    let mut environment = |name: &str| match name {
        "DASHSCOPE_API_KEY" => Ok("test-key".to_string()),
        "MODEL_API_KEY" => Err(env::VarError::NotUnicode("\u{fffd}".into())),
        _ => Err(env::VarError::NotPresent),
    };
    let mut warnings = Vec::new();

    // Act
    let catalog = model_catalog(ModelProvider::Qwen, None, &mut environment, &mut warnings)
        .expect("an unusable provider should not fail the chat");

    // Assert
    assert!(catalog.specs().next().is_none());
    let warnings = String::from_utf8(warnings).expect("warnings should be UTF-8");
    assert!(warnings.starts_with("warning: cannot catalog muse models: "));
    assert!(warnings.ends_with("; /model lists no models and turns report no cost\n"));
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
fn model_connector_reads_openai_compatible_profiles_from_the_environment() {
    // Arrange
    let mut environment = |name: &str| match name {
        "OPENAI_COMPATIBLE_CONTEXT_WINDOW" => Ok("40960".to_string()),
        "OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS" => Ok("many".to_string()),
        _ => Err(env::VarError::NotPresent),
    };
    let mut valid_environment = |name: &str| match name {
        "OPENAI_COMPATIBLE_CONTEXT_WINDOW" => Ok("40960".to_string()),
        "MODEL_API_KEY" => Ok("test-key".to_string()),
        _ => Err(env::VarError::NotPresent),
    };
    let local = ModelSelection {
        model: "qwen3:latest".to_string(),
        provider: ModelProvider::OpenAiCompatible,
    };

    // Act
    let invalid = model_connector(ModelProvider::OpenAiCompatible, None, &mut environment)(&local);
    let mut connect = model_connector(
        ModelProvider::OpenAiCompatible,
        None,
        &mut valid_environment,
    );
    let (_, local_profile) = connect(&local).expect("Ollama defaults should connect");
    let (_, hosted_profile) =
        connect(&muse_selection("muse-spark-1.3")).expect("Muse key should connect");

    // Assert
    assert!(matches!(invalid, Err(CliError::ModelConfiguration(_))));
    assert_eq!(local_profile.context_window_tokens(), 40_960);
    // The harness catalog declares Muse's profile.
    assert_eq!(hosted_profile.context_window_tokens(), 1_048_576);
    assert_ne!(hosted_profile, MODEL_PROFILE);
}

#[test]
fn model_identity_is_absent_beyond_the_identity_limit() {
    // Arrange
    let longest = muse_selection(&"m".repeat(256 - "muse/".len()));
    let oversized = muse_selection(&"m".repeat(256 - "muse/".len() + 1));
    let mut models = switcher(test_catalog(), longest.clone());

    // Act
    let identity = longest.identity();
    let missing = oversized.identity();

    // Assert
    let identity = identity.expect("a 256-byte key should be registrable");
    assert_eq!(identity.key(), longest.id().as_str());
    assert!(models.registry(identity, &longest).is_ok());
    assert_eq!(missing, None);
}
