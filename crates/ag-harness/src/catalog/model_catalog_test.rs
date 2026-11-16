//! Model catalog unit tests for building, selector resolution, price
//! overrides, and environment-derived models and role defaults.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::Path;
use std::sync::Arc;

use ag_router::chatgpt::ChatGptCredentials;

use crate::catalog::{
    Capabilities, CatalogError, ModelCatalog, ModelId, ModelSelector, ModelSpec, Pricing,
    PricingSource, Role,
};
use crate::context::ContextBudgetError;
use crate::model::{MockModel, Model, ModelProfile};
use crate::provider::{ModelConfigurationError, ModelProvider};

const TEXT: &str = "host/text";
const VISION: &str = "host/vision";

fn model() -> Arc<dyn Model> {
    Arc::new(MockModel::new())
}

fn text_spec() -> ModelSpec {
    ModelSpec::new(TEXT).with_capabilities(Capabilities::NONE.with_tool_calls(true))
}

fn vision_spec() -> ModelSpec {
    ModelSpec::new(VISION)
        .with_capabilities(
            Capabilities::NONE
                .with_image_input(true)
                .with_tool_calls(true),
        )
        .with_profile(ModelProfile::new(100_000).with_reserved_tokens(10_000))
}

/// Returns a catalog of a text model and a vision model, with the text
/// model first among the smart candidates.
fn catalog() -> ModelCatalog {
    ModelCatalog::builder()
        .model(text_spec(), model())
        .model(vision_spec(), model())
        .assign(Role::SMART, [TEXT, VISION])
        .assign(Role::CHEAP, [TEXT])
        .build()
        .expect("catalog should build")
}

fn lookup(variables: &[(&str, &str)]) -> impl FnMut(&str) -> Result<String, env::VarError> + use<> {
    let variables = variables
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect::<BTreeMap<_, _>>();

    move |name| {
        variables
            .get(name)
            .cloned()
            .ok_or(env::VarError::NotPresent)
    }
}

fn ids(candidates: &[ModelId]) -> Vec<&str> {
    candidates.iter().map(ModelId::as_str).collect()
}

fn save_sign_in(auth_file: &Path) {
    ChatGptCredentials {
        access_token: "access".to_string(),
        client_id: "oaiapp_test".to_string(),
        email: None,
        expires_at: u64::MAX,
        ext_agent_host_id: "urn:uuid:host".to_string(),
        id_token: String::new(),
        refresh_token: "refresh".to_string(),
        scopes: vec!["chatgpt.tokens.use.direct".to_string()],
    }
    .save(auth_file)
    .expect("credentials should save");
}

#[test]
fn resolves_models_by_identifier_role_and_required_capability() {
    // Arrange
    let catalog = catalog();
    let needs_images = ModelSelector::Require {
        needs: Capabilities::NONE.with_image_input(true),
        role: Role::SMART,
    };

    // Act
    let by_id = catalog
        .resolve(&ModelSelector::Id(ModelId::new(VISION)))
        .expect("cataloged identifier");
    let by_role = catalog
        .resolve(&ModelSelector::Role(Role::SMART))
        .expect("assigned role");
    let by_capability = catalog.resolve(&needs_images).expect("vision candidate");

    // Assert
    assert_eq!(by_id.id().as_str(), VISION);
    assert_eq!(by_id.spec(), &vision_spec());
    assert_eq!(by_role.id().as_str(), TEXT);
    assert_eq!(by_role.profile(), None);
    assert_eq!(by_role.context_budget(), None);
    assert_eq!(by_capability.id().as_str(), VISION);
    assert_eq!(by_capability.profile(), vision_spec().profile());
    assert_eq!(
        by_capability.context_budget(),
        vision_spec()
            .profile()
            .map(|profile| profile.context_budget().expect("valid profile"))
    );
    assert!(Arc::ptr_eq(&by_id.model(), &by_capability.model()));
    assert!(format!("{by_id:?}").contains("host/vision"));
}

#[test]
fn resolution_reports_unknown_models_unassigned_roles_and_missing_capabilities() {
    // Arrange
    let catalog = catalog();
    let needs_reasoning = ModelSelector::Require {
        needs: Capabilities::NONE.with_reasoning(true),
        role: Role::SMART,
    };

    // Act
    let unknown = catalog.resolve(&ModelSelector::Id(ModelId::new("host/missing")));
    let unassigned = catalog.resolve(&ModelSelector::Role(Role::FAST));
    let unassigned_require = catalog.resolve(&ModelSelector::Require {
        needs: Capabilities::NONE,
        role: Role::VISION,
    });
    let missing = catalog.resolve(&needs_reasoning);

    // Assert
    let unknown = unknown.expect_err("uncataloged identifier");
    assert!(matches!(&unknown, CatalogError::UnknownModel { id } if id.as_str() == "host/missing"));
    assert_eq!(unknown.to_string(), "model `host/missing` is not cataloged");
    let unassigned = unassigned.expect_err("role without candidates");
    assert!(matches!(&unassigned, CatalogError::UnassignedRole { role } if *role == Role::FAST));
    assert_eq!(
        unassigned.to_string(),
        "no model is assigned to the `fast` role"
    );
    assert!(matches!(
        unassigned_require,
        Err(CatalogError::UnassignedRole { role }) if role == Role::VISION
    ));
    let missing = missing.expect_err("no reasoning candidate");
    assert_eq!(
        missing.to_string(),
        "no `smart` model has the capabilities reasoning"
    );
}

#[test]
fn assignments_replace_earlier_ones_and_an_empty_assignment_removes_the_role() {
    // Arrange
    let builder = catalog().into_builder();

    // Act
    let catalog = builder
        .assign(Role::SMART, [VISION])
        .assign(Role::CHEAP, Vec::<ModelId>::new())
        .build()
        .expect("catalog should rebuild");

    // Assert
    assert_eq!(ids(catalog.candidates(&Role::SMART)), [VISION]);
    assert_eq!(catalog.candidates(&Role::CHEAP), []);
    assert_eq!(
        catalog
            .specs()
            .map(|spec| spec.id().as_str())
            .collect::<Vec<_>>(),
        [TEXT, VISION]
    );
    assert_eq!(
        catalog.spec(&ModelId::new(TEXT)).map(ModelSpec::id),
        Some(&ModelId::new(TEXT))
    );
    assert!(catalog.spec(&ModelId::new("host/missing")).is_none());
    assert_eq!(
        format!("{catalog:?}"),
        "ModelCatalog { models: [ModelId(\"host/text\"), ModelId(\"host/vision\")], roles: \
         {Role(\"smart\"): [ModelId(\"host/vision\")]} }"
    );
}

#[test]
fn host_pricing_replaces_the_declared_prices() {
    // Arrange
    let negotiated = Pricing::new(500_000, 1_500_000);

    // Act
    let catalog = ModelCatalog::builder()
        .model(text_spec().with_pricing(Pricing::built_in(1, 2)), model())
        .pricing(TEXT, negotiated)
        .build()
        .expect("catalog should build");

    // Assert
    let pricing = catalog
        .spec(&ModelId::new(TEXT))
        .and_then(ModelSpec::pricing)
        .expect("priced model");
    assert_eq!(pricing, negotiated);
    assert_eq!(pricing.source(), PricingSource::Host);
}

#[test]
fn build_rejects_duplicates_invalid_profiles_and_unknown_references() {
    // Arrange
    let invalid_profile = ModelSpec::new(TEXT).with_profile(ModelProfile::new(0));

    // Act
    let duplicate = ModelCatalog::builder()
        .model(text_spec(), model())
        .model(text_spec(), model())
        .build();
    let invalid = ModelCatalog::builder()
        .model(invalid_profile, model())
        .build();
    let unknown_candidate = ModelCatalog::builder()
        .model(text_spec(), model())
        .assign(Role::SMART, [VISION])
        .build();
    let unknown_pricing = ModelCatalog::builder()
        .pricing(VISION, Pricing::new(1, 1))
        .build();

    // Assert
    let duplicate = duplicate.expect_err("duplicate identifier");
    assert_eq!(
        duplicate.to_string(),
        "model `host/text` is cataloged twice"
    );
    let invalid = invalid.expect_err("empty window");
    assert_eq!(
        invalid.to_string(),
        "model `host/text` declares an invalid context profile"
    );
    assert!(matches!(
        invalid,
        CatalogError::InvalidProfile {
            source: ContextBudgetError::ReservedOutputExceedsBudget { .. },
            ..
        }
    ));
    for error in [unknown_candidate, unknown_pricing] {
        assert!(matches!(
            error,
            Err(CatalogError::UnknownModel { id }) if id.as_str() == VISION
        ));
    }
}

#[test]
fn environment_catalogs_configured_providers_with_default_roles() {
    // Arrange
    let environment = lookup(&[
        ("OPENAI_API_KEY", "openai-key"),
        ("DASHSCOPE_API_KEY", "qwen-key"),
        ("DASHSCOPE_BASE_URL", "https://dashscope.test/v1"),
        ("KIMI_API_KEY", "kimi-key"),
    ]);

    // Act
    let catalog = ModelCatalog::from_environment_with(environment).expect("valid environment");

    // Assert
    assert_eq!(
        catalog
            .specs()
            .map(|spec| spec.id().as_str())
            .collect::<Vec<_>>(),
        [
            "openai/gpt-5.3-codex",
            "openai/gpt-5.6-terra",
            "openai/gpt-6-astra",
            "openai/gpt-6-luna",
            "openai/gpt-6.1-sol",
            "qwen/qwen-plus",
            "qwen/qwen3.7-flash",
            "qwen/qwen3.8-flash",
            "qwen/qwen3.8-max",
        ]
    );
    assert_eq!(
        ids(catalog.candidates(&Role::SMART)),
        ["openai/gpt-6.1-sol", "qwen/qwen3.8-max"]
    );
    assert_eq!(
        ids(catalog.candidates(&Role::CHEAP)),
        [
            "openai/gpt-6-luna",
            "qwen/qwen3.8-flash",
            "qwen/qwen3.7-flash"
        ]
    );
    assert_eq!(
        ids(catalog.candidates(&Role::FAST)),
        [
            "openai/gpt-6-luna",
            "qwen/qwen3.8-flash",
            "qwen/qwen3.7-flash"
        ]
    );
    assert_eq!(
        ids(catalog.candidates(&Role::VISION)),
        ["openai/gpt-6.1-sol", "qwen/qwen3.8-max"]
    );
    let smart = catalog
        .resolve(&ModelSelector::Role(Role::SMART))
        .expect("default smart model");
    assert_eq!(smart.spec().provider(), Some(ModelProvider::OpenAi));
    assert_eq!(smart.spec().context_window(), Some(1_050_000));
    assert!(smart.context_budget().is_some());
    assert_eq!(
        smart
            .model()
            .metadata()
            .map(|metadata| metadata.model().to_string()),
        Some("gpt-6.1-sol".to_string())
    );
}

#[test]
fn environment_prefers_plan_billed_chatgpt_models_when_signed_in() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("chatgpt.json");
    save_sign_in(&auth_file);
    let auth_file = auth_file.to_string_lossy().into_owned();
    let environment = lookup(&[
        ("CHATGPT_AUTH_FILE", auth_file.as_str()),
        ("MODEL_API_KEY", "muse-key"),
    ]);

    // Act
    let catalog = ModelCatalog::from_environment_with(environment).expect("valid environment");

    // Assert
    assert_eq!(catalog.specs().count(), 10);
    assert_eq!(
        ids(catalog.candidates(&Role::SMART)),
        ["chatgpt/gpt-6.1-sol", "muse/muse-spark-1.3"]
    );
    assert_eq!(
        ids(catalog.candidates(&Role::CHEAP)),
        ["chatgpt/gpt-6-luna"]
    );
    assert_eq!(
        catalog
            .spec(&ModelId::new("chatgpt/gpt-6-luna"))
            .and_then(ModelSpec::pricing),
        None
    );
    assert!(
        catalog
            .spec(&ModelId::new("muse/muse-spark-1.3-contributor"))
            .is_some()
    );
}

#[test]
fn empty_environment_catalogs_nothing() {
    // Arrange
    let environment = lookup(&[]);

    // Act
    let catalog = ModelCatalog::from_environment_with(environment).expect("nothing configured");

    // Assert
    assert_eq!(catalog.specs().count(), 0);
    assert!(matches!(
        catalog.resolve(&ModelSelector::Role(Role::SMART)),
        Err(CatalogError::UnassignedRole { .. })
    ));
}

#[test]
fn environment_reports_an_invalid_configured_provider() {
    // Arrange
    let environment = |name: &str| match name {
        "OPENAI_API_KEY" => Ok("openai-key".to_string()),
        "OPENAI_BASE_URL" => Err(env::VarError::NotUnicode(OsString::from_vec(vec![0xff]))),
        _ => Err(env::VarError::NotPresent),
    };

    // Act
    let error = ModelCatalog::from_environment_with(environment).expect_err("invalid base URL");

    // Assert
    assert_eq!(error.to_string(), "cannot catalog openai models");
    assert!(matches!(
        error,
        CatalogError::Configuration {
            provider: ModelProvider::OpenAi,
            source: ModelConfigurationError::Environment { .. },
        }
    ));
}

#[test]
fn process_environment_catalog_reads_the_process_environment() {
    // Arrange and Act
    let catalog = ModelCatalog::from_environment();

    // Assert
    if let Ok(catalog) = catalog {
        assert!(catalog.specs().all(|spec| spec.provider().is_some()));
    }
}
