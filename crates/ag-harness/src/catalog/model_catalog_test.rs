//! Model catalog unit tests for building, role requirements, selector
//! resolution, price overrides, and environment-derived models and role
//! defaults. Environment-derived catalogs need both provider families.

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
use std::collections::BTreeMap;
#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
use std::ffi::OsString;
#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
use std::os::unix::ffi::OsStringExt as _;
use std::sync::Arc;
#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
use std::{env, fs};

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
use ag_router::chatgpt::ChatGptAuthError;

use crate::catalog::{
    Capabilities, CatalogError, ModelCatalog, ModelId, ModelSelector, ModelSpec, Pricing,
    PricingSource, Role,
};
#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
use crate::chatgpt_sign_in_fixture::save_chatgpt_sign_in;
use crate::context::ContextBudgetError;
use crate::model::{MockModel, Model, ModelProfile};
#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
use crate::provider::{ModelConfigurationError, ModelProvider};

const TEXT: &str = "host/text";
const VISION: &str = "host/vision";

/// Default-role table over host models: a text-only profiled model, an
/// unprofiled vision model, an uncataloged model, and a profiled vision
/// model.
#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
const HOST_ROLES: &[(Role, &[(ModelProvider, &str)])] = &[
    (
        Role::VISION,
        &[
            (ModelProvider::OpenAiCompatible, "text"),
            (ModelProvider::OpenAiCompatible, "unprofiled-vision"),
            (ModelProvider::OpenAiCompatible, "missing"),
            (ModelProvider::OpenAiCompatible, "vision"),
        ],
    ),
    (
        Role::SMART,
        &[
            (ModelProvider::OpenAiCompatible, "missing"),
            (ModelProvider::OpenAiCompatible, "text"),
        ],
    ),
];

/// Returns a model that declares `profile`.
fn profiled(profile: Option<ModelProfile>) -> Arc<dyn Model> {
    let mut model = MockModel::new();
    model.expect_profile().return_const(profile);

    Arc::new(model)
}

/// Returns a model that declares no profile.
fn model() -> Arc<dyn Model> {
    profiled(None)
}

fn host_profile() -> ModelProfile {
    ModelProfile::new(100_000).with_reserved_tokens(10_000)
}

fn image_input() -> Capabilities {
    Capabilities::NONE.with_image_input(true)
}

fn text_spec() -> ModelSpec {
    ModelSpec::new(TEXT).with_capabilities(Capabilities::NONE.with_tool_calls(true))
}

fn vision_spec() -> ModelSpec {
    ModelSpec::new(VISION)
        .with_capabilities(image_input().with_tool_calls(true))
        .with_profile(host_profile())
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

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
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

fn spec_ids(catalog: &ModelCatalog) -> Vec<&str> {
    catalog.specs().map(|spec| spec.id().as_str()).collect()
}

#[test]
fn resolves_models_by_identifier_role_and_required_capability() {
    // Arrange
    let catalog = catalog();
    let needs_images = ModelSelector::Require {
        needs: image_input(),
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
    assert_eq!(by_capability.profile(), Some(host_profile()));
    assert_eq!(
        by_capability.context_budget(),
        Some(host_profile().context_budget().expect("valid profile"))
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
    assert_eq!(spec_ids(&catalog), [TEXT, VISION]);
    assert_eq!(
        catalog.spec(&ModelId::new(TEXT)).map(ModelSpec::id),
        Some(&ModelId::new(TEXT))
    );
    assert!(catalog.spec(&ModelId::new("host/missing")).is_none());
    assert_eq!(
        format!("{catalog:?}"),
        "ModelCatalog { models: [ModelId(\"host/text\"), ModelId(\"host/vision\")], requirements: \
         {Role(\"vision\"): Capabilities { flags: 1 }}, roles: {Role(\"smart\"): \
         [ModelId(\"host/vision\")]} }"
    );
}

#[test]
fn role_requirements_reject_incapable_candidates_and_are_replaceable() {
    // Arrange
    let builder = || {
        ModelCatalog::builder()
            .model(text_spec(), model())
            .model(vision_spec(), model())
    };

    // Act
    let text_vision = builder().assign(Role::VISION, [TEXT]).build();
    let text_review = builder()
        .require(Role::new("review"), Capabilities::NONE.with_reasoning(true))
        .assign(Role::new("review"), [TEXT])
        .build();
    let relaxed = builder()
        .require(Role::VISION, Capabilities::NONE)
        .assign(Role::VISION, [TEXT])
        .build()
        .expect("vision requirement removed");
    let vision = builder()
        .assign(Role::VISION, [VISION])
        .build()
        .expect("vision candidate reads images");

    // Assert
    let text_vision = text_vision.expect_err("text model cannot be a vision candidate");
    assert_eq!(
        text_vision.to_string(),
        "model `host/text` lacks the capabilities image_input the `vision` role requires"
    );
    assert!(matches!(
        text_review,
        Err(CatalogError::IncapableCandidate { id, needs, role })
            if id.as_str() == TEXT
                && needs == Capabilities::NONE.with_reasoning(true)
                && role == Role::new("review")
    ));
    assert_eq!(relaxed.requirement(&Role::VISION), Capabilities::NONE);
    assert_eq!(vision.requirement(&Role::VISION), image_input());
    assert_eq!(
        vision
            .into_builder()
            .build()
            .map(|catalog| catalog.requirement(&Role::VISION))
            .ok(),
        Some(image_input())
    );
    assert_eq!(catalog().requirement(&Role::SMART), Capabilities::NONE);
}

#[test]
fn specifications_without_a_profile_adopt_the_model_profile() {
    // Arrange
    let catalog = ModelCatalog::builder()
        .model(ModelSpec::new(TEXT), profiled(Some(host_profile())))
        .assign(Role::SMART, [TEXT])
        .build()
        .expect("catalog should build");

    // Act
    let handle = catalog
        .resolve(&ModelSelector::Role(Role::SMART))
        .expect("assigned role");

    // Assert
    assert_eq!(handle.profile(), Some(host_profile()));
    assert_eq!(handle.spec().context_window(), Some(100_000));
    assert_eq!(
        handle.context_budget(),
        Some(host_profile().context_budget().expect("valid profile"))
    );
}

#[test]
fn host_pricing_replaces_the_declared_prices_as_host_prices() {
    // Arrange
    let negotiated = Pricing::built_in(500_000, 1_500_000);

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
    assert_eq!(pricing.input_per_mtok_micro_usd(), 500_000);
    assert_eq!(pricing.output_per_mtok_micro_usd(), 1_500_000);
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

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn default_roles_keep_only_cataloged_profiled_and_capable_candidates() {
    // Arrange
    let host = |model: &str| ModelId::cataloged(ModelProvider::OpenAiCompatible, model);
    let catalog = ModelCatalog::builder()
        .model(ModelSpec::new(host("text")), profiled(Some(host_profile())))
        .model(
            ModelSpec::new(host("unprofiled-vision")).with_capabilities(image_input()),
            model(),
        )
        .model(
            ModelSpec::new(host("vision")).with_capabilities(image_input()),
            profiled(Some(host_profile())),
        )
        .build()
        .expect("catalog should build");

    // Act
    let catalog = ModelCatalog::assign_default_roles(catalog, HOST_ROLES)
        .build()
        .expect("default roles should build");

    // Assert
    assert_eq!(
        ids(catalog.candidates(&Role::VISION)),
        ["openai-compatible/vision"]
    );
    assert_eq!(
        ids(catalog.candidates(&Role::SMART)),
        ["openai-compatible/text"]
    );
}

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn environment_catalogs_configured_providers_with_default_roles() {
    // Arrange
    let environment = lookup(&[
        ("OPENAI_API_KEY", "openai-key"),
        ("DASHSCOPE_API_KEY", "qwen-key"),
        ("DASHSCOPE_BASE_URL", "https://dashscope.test/v1"),
        ("KIMI_API_KEY", "kimi-key"),
        ("KIMI_BASE_URL", "https://kimi.test/v1"),
    ]);

    // Act
    let catalog = ModelCatalog::from_environment_with(environment).expect("valid environment");

    // Assert
    assert_eq!(
        spec_ids(&catalog),
        [
            "kimi/kimi-k2.6",
            "kimi/kimi-k3",
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
        ["openai/gpt-6.1-sol"]
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
        ["openai/gpt-6.1-sol"]
    );
    for role in [Role::SMART, Role::CHEAP, Role::FAST, Role::VISION] {
        for id in catalog.candidates(&role) {
            let handle = catalog
                .resolve(&ModelSelector::Id(id.clone()))
                .expect("cataloged candidate");
            assert!(handle.context_budget().is_some(), "{role} candidate {id}");
        }
    }
    let smart = catalog
        .resolve(&ModelSelector::Role(Role::SMART))
        .expect("default smart model");
    assert_eq!(smart.spec().provider(), Some(ModelProvider::OpenAi));
    assert_eq!(smart.spec().context_window(), Some(1_050_000));
    assert_eq!(
        smart
            .model()
            .metadata()
            .map(|metadata| metadata.model().to_string()),
        Some("gpt-6.1-sol".to_string())
    );
}

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn environment_skips_a_provider_whose_required_base_url_is_absent() {
    // Arrange
    let environment = lookup(&[("KIMI_API_KEY", "kimi-key")]);

    // Act
    let catalog = ModelCatalog::from_environment_with(environment).expect("valid environment");

    // Assert
    assert_eq!(catalog.specs().count(), 0);
}

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn environment_prefers_plan_billed_chatgpt_models_when_signed_in() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("chatgpt.json");
    save_chatgpt_sign_in(&auth_file);
    let auth_file = auth_file.to_string_lossy().into_owned();
    let environment = lookup(&[
        ("CHATGPT_AUTH_FILE", auth_file.as_str()),
        ("MODEL_API_KEY", "muse-key"),
    ]);

    // Act
    let catalog = ModelCatalog::from_environment_with(environment).expect("valid environment");

    // Assert
    assert_eq!(catalog.specs().count(), 9);
    assert_eq!(
        ids(catalog.candidates(&Role::SMART)),
        ["chatgpt/gpt-6.1-sol"]
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
    assert!(catalog.spec(&ModelId::new("muse/muse-spark-1.3")).is_some());
    assert!(
        catalog
            .spec(&ModelId::new("muse/muse-spark-1.3-contributor"))
            .is_none()
    );
}

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn environment_skips_chatgpt_without_a_stored_sign_in() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("missing.json");
    let auth_file = auth_file.to_string_lossy().into_owned();
    let environment = lookup(&[("CHATGPT_AUTH_FILE", auth_file.as_str())]);

    // Act
    let catalog = ModelCatalog::from_environment_with(environment).expect("not signed in");

    // Assert
    assert_eq!(catalog.specs().count(), 0);
}

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn environment_reports_a_broken_chatgpt_sign_in() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("chatgpt.json");
    fs::write(&auth_file, "not json").expect("corrupt credentials should save");
    let auth_file = auth_file.to_string_lossy().into_owned();
    let environment = lookup(&[("CHATGPT_AUTH_FILE", auth_file.as_str())]);

    // Act
    let error = ModelCatalog::from_environment_with(environment).expect_err("corrupt sign-in");

    // Assert
    assert!(matches!(
        error,
        CatalogError::Configuration {
            provider: ModelProvider::ChatGpt,
            source: ModelConfigurationError::SignIn(ChatGptAuthError::InvalidCredentials { .. }),
        }
    ));
}

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
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

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn environment_reports_an_invalid_configured_provider() {
    // Arrange
    let invalid_base_url = |name: &str| match name {
        "OPENAI_API_KEY" => Ok("openai-key".to_string()),
        "OPENAI_BASE_URL" => Err(env::VarError::NotUnicode(OsString::from_vec(vec![0xff]))),
        _ => Err(env::VarError::NotPresent),
    };
    let invalid_api_key = |name: &str| match name {
        "OPENAI_API_KEY" => Err(env::VarError::NotUnicode(OsString::from_vec(vec![0xff]))),
        _ => Err(env::VarError::NotPresent),
    };

    // Act
    let base_url = ModelCatalog::from_environment_with(invalid_base_url).expect_err("base URL");
    let api_key = ModelCatalog::from_environment_with(invalid_api_key).expect_err("API key");

    // Assert
    assert_eq!(base_url.to_string(), "cannot catalog openai models");
    assert!(matches!(
        base_url,
        CatalogError::Configuration {
            provider: ModelProvider::OpenAi,
            source: ModelConfigurationError::Environment { .. },
        }
    ));
    assert!(matches!(
        api_key,
        CatalogError::Configuration {
            provider: ModelProvider::OpenAi,
            source: ModelConfigurationError::ApiKey { .. },
        }
    ));
}

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn process_environment_catalog_delegates_to_the_process_environment() {
    // Arrange
    let catalog_ids = |catalog: Result<ModelCatalog, CatalogError>| -> Result<Vec<String>, String> {
        catalog
            .map(|catalog| catalog.specs().map(|spec| spec.id().to_string()).collect())
            .map_err(|error| error.to_string())
    };

    // Act
    let process = catalog_ids(ModelCatalog::from_environment());
    let injected = catalog_ids(ModelCatalog::from_environment_with(|name| env::var(name)));

    // Assert
    assert_eq!(process, injected);
}
