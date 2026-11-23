//! `HarnessBuilder` unit tests for model selection, budget derivation,
//! validation failures, applied options, and shared handle configuration.

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
use std::env;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::support::{model, object_schema, response_without_metadata};
use crate::calibration::Calibration;
use crate::catalog::{
    Capabilities, CatalogError, ModelCatalog, ModelId, ModelSelector, ModelSpec, Pricing, Role,
};
use crate::context::{
    ContextBudget, ContextBudgetError, ContextEstimator, ContextSummaryPolicy,
    HeuristicContextEstimator,
};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::error::{ErrorKind, HarnessError};
use crate::file_system::MockFileSystem;
use crate::harness::{DEFAULT_MAX_HISTORY_BYTES, Harness, HarnessBuildError, HistoryBudget};
use crate::lifecycle::LifecycleEvent;
use crate::memory_store::MemoryStore;
use crate::model::{
    MockModel, Model, ModelError, ModelMetadata, ModelProfile, ModelResponse, ReasoningEffort,
};
use crate::model_registry::{ModelCapabilities, ModelRegistry};
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
use crate::provider::ModelProvider;
use crate::recovery::ExecutionIdentity;
use crate::repository::Repository;
use crate::store::{SessionStore, StoreBackend as _};
use crate::summary_model::SummaryModel;
use crate::tool::{
    ToolContext, ToolDefinition, ToolEffects, ToolError, ToolHandler, ToolOutput, ToolSet,
};

/// Request weight declared by the profile estimator, distinguishing it
/// from the default estimator's zero request cost.
const PROFILE_REQUEST_TOKENS: u64 = 77;

/// Canonical path the configured filesystem double resolves every path to.
const SENTINEL_PATH: &str = "/configured/file-system";

fn profiled_model(profile: Option<ModelProfile>) -> MockModel {
    let mut model = model();
    model.expect_profile().return_const(profile);

    model
}

fn declared_profile() -> ModelProfile {
    ModelProfile::new(10_000)
        .with_reserved_tokens(1_000)
        .with_estimator(
            HeuristicContextEstimator::DEFAULT.with_request_tokens(PROFILE_REQUEST_TOKENS),
        )
}

/// Returns the configuration error of a failed build after checking its
/// classification.
fn build_error(result: Result<Harness, HarnessError>) -> HarnessError {
    let error = result.err().expect("build should fail");
    assert_eq!(error.kind(), ErrorKind::Config);
    assert!(!error.is_retryable());

    error
}

#[test]
fn build_without_a_model_reports_model_required() {
    // Arrange
    let builder = Harness::builder().context_budget(unbounded_context_budget());

    // Act
    let error = build_error(builder.build());

    // Assert
    assert!(matches!(
        error.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::ModelRequired)
    ));
}

#[test]
fn build_with_an_invalid_declared_profile_reports_it() {
    // Arrange
    let profile = ModelProfile::new(100).with_reserved_tokens(100);
    let builder = Harness::builder().model(Arc::new(profiled_model(Some(profile))));

    // Act
    let error = build_error(builder.build());

    // Assert
    assert!(matches!(
        error.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::InvalidProfile(
            ContextBudgetError::ReservedOutputExceedsBudget {
                max_request_weight: 100,
                reserved_output_weight: 100,
            }
        ))
    ));
}

#[test]
fn build_with_repository_tools_but_no_repository_reports_it() {
    // Arrange
    let tool_sets = [
        ToolSet::empty().with_bash(),
        ToolSet::empty().with_read(),
        ToolSet::empty().with_write(),
    ];
    let builders = tool_sets.map(|tools| {
        Harness::builder()
            .model(Arc::new(profiled_model(Some(declared_profile()))))
            .tools(tools)
    });

    // Act
    let errors = builders.map(|builder| build_error(builder.build()));

    // Assert
    for error in errors {
        assert!(matches!(
            error.get_ref().downcast_ref::<HarnessBuildError>(),
            Some(HarnessBuildError::RepositoryRequired)
        ));
    }
}

#[test]
fn build_derives_budget_and_estimator_from_the_declared_profile() {
    // Arrange
    let profile = declared_profile();
    let builder = Harness::builder().model(Arc::new(profiled_model(Some(profile))));

    // Act
    let harness = builder.build().expect("profiled model should build");

    // Assert
    assert_eq!(
        harness.inner.context_budget,
        profile.context_budget().expect("valid profile")
    );
    assert_eq!(
        harness
            .inner
            .context_estimator
            .request_weight(Some(&object_schema())),
        PROFILE_REQUEST_TOKENS
    );
    assert!(harness.model_registration().is_none());
}

#[test]
fn explicit_budget_overrides_the_declared_profile() {
    // Arrange
    let budget = ContextBudget::new(NonZeroU64::new(500).expect("non-zero"));
    let builder = Harness::builder()
        .context_budget(budget)
        .model(Arc::new(profiled_model(Some(declared_profile()))));

    // Act
    let harness = builder.build().expect("budgeted model should build");

    // Assert
    assert_eq!(harness.inner.context_budget, budget);
    assert_eq!(
        harness
            .inner
            .context_estimator
            .request_weight(Some(&object_schema())),
        PROFILE_REQUEST_TOKENS
    );
}

#[test]
fn build_resolves_the_catalog_selector_under_the_cataloged_profile() {
    // Arrange
    let profile = declared_profile();
    let smart: Arc<dyn Model> = Arc::new(profiled_model(None));
    let cheap: Arc<dyn Model> = Arc::new(model());
    let catalog = ModelCatalog::builder()
        .model(ModelSpec::new("host/smart"), Arc::clone(&smart))
        .model(
            ModelSpec::new("host/cheap").with_profile(profile),
            Arc::clone(&cheap),
        )
        .assign(Role::SMART, ["host/smart"])
        .assign(Role::CHEAP, ["host/cheap"])
        .build()
        .expect("catalog should build");

    // Act
    let harness = Harness::builder()
        .catalog(Arc::new(catalog))
        .model_selector(Role::CHEAP)
        .build()
        .expect("cataloged model should build");

    // Assert
    assert!(Arc::ptr_eq(&harness.inner.model, &cheap));
    assert_eq!(
        harness.inner.context_budget,
        profile.context_budget().expect("valid profile")
    );
    assert_eq!(
        harness
            .inner
            .context_estimator
            .request_weight(Some(&object_schema())),
        PROFILE_REQUEST_TOKENS
    );
}

#[test]
fn build_defaults_the_catalog_selector_to_the_smart_role_and_model_profile() {
    // Arrange
    let profile = declared_profile();
    let smart: Arc<dyn Model> = Arc::new(profiled_model(Some(profile)));
    let catalog = ModelCatalog::builder()
        .model(ModelSpec::new("host/smart"), Arc::clone(&smart))
        .assign(Role::SMART, ["host/smart"])
        .build()
        .expect("catalog should build");

    // Act
    let harness = Harness::builder()
        .catalog(Arc::new(catalog))
        .build()
        .expect("smart model should build");

    // Assert
    assert!(Arc::ptr_eq(&harness.inner.model, &smart));
    assert_eq!(
        harness.inner.context_budget,
        profile.context_budget().expect("valid profile")
    );
}

#[test]
fn explicit_model_takes_precedence_over_the_catalog_but_not_a_selector() {
    // Arrange
    let explicit: Arc<dyn Model> = Arc::new(profiled_model(Some(declared_profile())));
    let empty = Arc::new(ModelCatalog::builder().build().expect("empty catalog"));

    // Act
    let harness = Harness::builder()
        .catalog(Arc::clone(&empty))
        .model(Arc::clone(&explicit))
        .build()
        .expect("explicit model should build");
    let conflicting = build_error(
        Harness::builder()
            .catalog(empty)
            .model_selector(Role::VISION)
            .model(Arc::clone(&explicit))
            .build(),
    );

    // Assert
    assert!(Arc::ptr_eq(&harness.inner.model, &explicit));
    assert!(matches!(
        conflicting.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::ConflictingModelSelection)
    ));
}

#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn environment_catalog_defaults_build_without_an_explicit_budget() {
    // Arrange
    let environment = |name: &str| match name {
        "DASHSCOPE_API_KEY" => Ok("qwen-key".to_string()),
        "DASHSCOPE_BASE_URL" => Ok("https://dashscope.test/v1".to_string()),
        _ => Err(env::VarError::NotPresent),
    };
    let catalog =
        Arc::new(ModelCatalog::from_environment_with(environment).expect("valid Qwen environment"));

    // Act
    let cheap = Harness::builder()
        .catalog(Arc::clone(&catalog))
        .model_selector(Role::CHEAP)
        .build();
    let smart = build_error(Harness::builder().catalog(catalog).build());

    // Assert
    assert!(cheap.is_ok());
    assert!(matches!(
        smart.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::Catalog(CatalogError::UnassignedRole { role }))
            if *role == Role::SMART
    ));
}

#[test]
fn build_reports_a_selector_without_catalog_and_an_unresolved_selector() {
    // Arrange
    let empty = Arc::new(ModelCatalog::builder().build().expect("empty catalog"));

    // Act
    let uncataloged = build_error(Harness::builder().model_selector(Role::FAST).build());
    let unresolved = build_error(Harness::builder().catalog(empty).build());

    // Assert
    assert!(matches!(
        uncataloged.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::CatalogRequired)
    ));
    assert!(matches!(
        unresolved.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::Catalog(CatalogError::UnassignedRole { role }))
            if *role == Role::SMART
    ));
}

#[tokio::test]
async fn build_applies_every_configured_option() {
    // Arrange
    let identity = ExecutionIdentity::new("host", "2").expect("identity");
    let estimator = HeuristicContextEstimator::DEFAULT.with_request_tokens(5);
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .returning(|_| Ok(PathBuf::from(SENTINEL_PATH)));
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());

    // Act
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(declared_profile()))))
        .store(Arc::clone(&store))
        .repository(Repository::fixture("root"))
        .tools(ToolSet::empty().with_read().with_write())
        .file_system(file_system)
        .max_history_bytes(NonZeroUsize::new(64).expect("non-zero"))
        .model_reasoning_effort(ReasoningEffort::High)
        .context_estimator(estimator)
        .context_summaries(ContextSummaryPolicy::Enabled)
        .execution_identity(identity.clone())
        .build()
        .expect("configured harness should build");

    // Assert
    let config = &harness.inner;
    assert!(
        config
            .store
            .as_ref()
            .is_some_and(|configured| Arc::ptr_eq(configured, &store))
    );
    assert_eq!(config.repository, Some(Repository::fixture("root")));
    assert_eq!(
        config.tools.resolve().names().collect::<Vec<_>>(),
        ["read", "write"]
    );
    assert_eq!(
        config
            .file_system
            .canonicalize(Path::new("any"))
            .await
            .expect("configured filesystem"),
        PathBuf::from(SENTINEL_PATH)
    );
    assert_eq!(config.history_budget, HistoryBudget::Fixed(64));
    assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(
        config
            .context_estimator
            .request_weight(Some(&object_schema())),
        5
    );
    assert_eq!(config.context_summaries, ContextSummaryPolicy::Enabled);
    assert_eq!(config.execution_identity, Some(identity));
}

#[tokio::test]
async fn built_history_budget_reaches_new_sessions() {
    // Arrange
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(declared_profile()))))
        .store(Arc::new(MemoryStore::new()))
        .max_history_bytes(NonZeroUsize::new(64).expect("non-zero"))
        .build()
        .expect("store-backed harness should build");

    // Act
    let session = harness
        .session("bounded", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Assert
    assert_eq!(session.history.max_bytes, 64);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_path_replaces_an_earlier_store() {
    // Act
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(declared_profile()))))
        .store(Arc::new(MemoryStore::new()))
        .sqlite("sessions.db")
        .build()
        .expect("SQLite-backed harness should build");

    // Assert
    assert_eq!(
        harness.inner.database_path,
        Some(PathBuf::from("sessions.db"))
    );
    assert!(harness.inner.store.is_none());
}

#[tokio::test]
async fn declared_context_window_sets_the_stored_history_budget_of_new_sessions() {
    // Arrange
    let store = Arc::new(MemoryStore::new());
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(declared_profile()))))
        .store(Arc::clone(&store) as Arc<dyn SessionStore>)
        .build()
        .expect("store-backed harness should build");
    let revised = Harness::builder()
        .model(Arc::new(profiled_model(Some(
            ModelProfile::new(100_000).with_reserved_tokens(1_000),
        ))))
        .store(Arc::clone(&store) as Arc<dyn SessionStore>)
        .build()
        .expect("revised harness should build");

    // Act
    let session = harness
        .session("windowed", object_schema())
        .system_prompt("be brief")
        .create()
        .await
        .expect("session should be created");
    let resumed = revised
        .resume("windowed")
        .await
        .expect("session should resume");

    // Assert
    // Of the 9,000 tokens left after 1,000 reserved, uncalibrated requests
    // fill 8,100; 900 stay for the input, 77 for the request, and 6 for the
    // system prompt, and the other 7,117 hold four bytes each.
    assert_eq!(session.history.max_bytes, 28_468);
    let stored = store
        .load_session("windowed")
        .await
        .expect("stored session");
    assert_eq!(stored.max_history_bytes, 28_468);
    assert_eq!(resumed.history.max_bytes, 28_468);
}

#[tokio::test]
async fn large_context_window_stops_at_the_safety_cap() {
    // Arrange
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(ModelProfile::new(1_000_000)))))
        .store(Arc::new(MemoryStore::new()))
        .build()
        .expect("store-backed harness should build");

    // Act
    let session = harness
        .session("large", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Assert
    assert_eq!(session.history.max_bytes, DEFAULT_MAX_HISTORY_BYTES);
}

#[tokio::test]
async fn context_window_without_room_for_history_keeps_the_default_budget() {
    // Arrange
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(
            ModelProfile::new(1_000).with_reserved_tokens(500),
        ))))
        .store(Arc::new(MemoryStore::new()))
        .build()
        .expect("store-backed harness should build");

    // Act
    let session = harness
        .session("crowded", object_schema())
        .system_prompt("be brief ".repeat(400))
        .create()
        .await
        .expect("session should be created");

    // Assert
    assert_eq!(session.history.max_bytes, DEFAULT_MAX_HISTORY_BYTES);
}

#[tokio::test]
async fn explicit_context_budget_keeps_the_default_history_budget() {
    // Arrange
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(declared_profile()))))
        .context_budget(declared_profile().context_budget().expect("budget"))
        .store(Arc::new(MemoryStore::new()))
        .build()
        .expect("budgeted harness should build");

    // Act
    let session = harness
        .session("explicit", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Assert
    assert_eq!(session.history.max_bytes, DEFAULT_MAX_HISTORY_BYTES);
}

#[tokio::test]
async fn registered_context_window_sets_the_history_budget_unless_overridden() {
    // Arrange
    let mut registry = ModelRegistry::new();
    registry
        .register(
            ExecutionIdentity::new("windowed", "1").expect("identity"),
            model(),
            ModelCapabilities::new(declared_profile()),
        )
        .expect("register");
    let derived = Harness::from_registry(&registry, "windowed")
        .expect("registered harness")
        .store(Arc::new(MemoryStore::new()));
    let overridden = Harness::from_registry(&registry, "windowed")
        .expect("registered harness")
        .max_history_bytes(NonZeroUsize::new(64).expect("history budget"))
        .store(Arc::new(MemoryStore::new()));

    // Act
    let derived = derived
        .session("derived", object_schema())
        .create()
        .await
        .expect("session should be created");
    let overridden = overridden
        .session("overridden", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Assert
    // 7,200 left for history less 77 request tokens, at four bytes each.
    assert_eq!(derived.history.max_bytes, 28_492);
    assert_eq!(overridden.history.max_bytes, 64);
}

#[tokio::test]
async fn the_window_history_budget_weighs_the_default_set_definitions() {
    // Arrange
    let host: Arc<dyn ToolHandler> = Arc::new(Clock);
    let hosted = ToolSet::builder()
        .handler(Arc::clone(&host))
        .build()
        .expect("tool set");
    let estimator = HeuristicContextEstimator::DEFAULT;
    let cases = [
        (hosted, estimator.tool_definition_weight(&host.definition())),
        (
            ToolSet::empty().with_read(),
            estimator.tool_definition_weight(&ToolDefinition::read_with_comparison_base(None)),
        ),
    ];
    let no_tools = windowed_session_history_bytes(|harness| harness).await;

    for (tools, weight) in cases {
        // Act
        let with_tools = windowed_session_history_bytes(move |harness| harness.tools(tools)).await;

        // Assert
        // Four history bytes per token of every advertised definition.
        assert_eq!(
            no_tools - with_tools,
            4 * usize::try_from(weight).expect("weight")
        );
    }
}

/// Returns the retained history bytes of a new session on a registered
/// harness with a declared context window, after `configure`.
async fn windowed_session_history_bytes(configure: impl FnOnce(Harness) -> Harness) -> usize {
    let mut registry = ModelRegistry::new();
    registry
        .register(
            ExecutionIdentity::new("windowed", "1").expect("identity"),
            model(),
            ModelCapabilities::new(declared_profile()),
        )
        .expect("register");
    let harness = Harness::from_registry(&registry, "windowed")
        .expect("registered harness")
        .store(Arc::new(MemoryStore::new()));

    configure(harness)
        .session("windowed", object_schema())
        .create()
        .await
        .expect("session should be created")
        .history
        .max_bytes
}

/// A host tool whose definition the history budget weighs; it never runs.
struct Clock;

#[async_trait]
impl ToolHandler for Clock {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "clock",
            "Return the current time.",
            json!({"type": "object"}),
        )
        .expect("valid definition")
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::ReadOnly
    }

    async fn call(
        &self,
        _arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        unreachable!("the history budget never runs a tool")
    }
}

#[tokio::test]
async fn undeclared_context_window_keeps_the_default_history_budget() {
    // Arrange
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(None)))
        .context_budget(unbounded_context_budget())
        .store(Arc::new(MemoryStore::new()))
        .build()
        .expect("budgeted harness should build");

    // Act
    let session = harness
        .session("unprofiled", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Assert
    assert_eq!(session.history.max_bytes, DEFAULT_MAX_HISTORY_BYTES);
}

#[tokio::test]
async fn byte_blind_estimator_keeps_the_default_history_budget() {
    // Arrange
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(declared_profile()))))
        .context_estimator(HeuristicContextEstimator::DEFAULT.with_messages(0, 4))
        .store(Arc::new(MemoryStore::new()))
        .build()
        .expect("store-backed harness should build");

    // Act
    let session = harness
        .session("byte-blind", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Assert
    assert_eq!(session.history.max_bytes, DEFAULT_MAX_HISTORY_BYTES);
}

#[cfg(feature = "sqlite")]
#[test]
fn store_replaces_an_earlier_sqlite_path() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());

    // Act
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(declared_profile()))))
        .sqlite("sessions.db")
        .store(Arc::clone(&store))
        .build()
        .expect("store-backed harness should build");

    // Assert
    assert!(harness.inner.database_path.is_none());
    assert!(
        harness
            .inner
            .store
            .as_ref()
            .is_some_and(|configured| Arc::ptr_eq(configured, &store))
    );
}

#[tokio::test]
async fn built_harness_reports_turns_to_its_lifecycle_observer() {
    // Arrange
    let mut model = profiled_model(None);
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::Output(
            json!({"summary": "done"}),
        )))
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&events);
    let harness = Harness::builder()
        .model(Arc::new(model))
        .context_budget(unbounded_context_budget())
        .lifecycle_observer(move |event: LifecycleEvent| {
            recorded.lock().expect("events lock").push(event);
        })
        .build()
        .expect("observed harness should build");

    // Act
    let outcome = harness
        .run_once("Summarize", object_schema())
        .await
        .expect("turn should succeed");

    // Assert
    assert_eq!(outcome.output(), &json!({"summary": "done"}));
    assert!(!events.lock().expect("events lock").is_empty());
}

#[test]
fn reconfiguring_a_clone_leaves_the_shared_configuration_unchanged() {
    // Arrange
    let harness = Harness::new(model(), unbounded_context_budget());
    let shared = harness.clone();
    assert!(Arc::ptr_eq(&harness.inner, &shared.inner));

    // Act
    let reconfigured = shared.tools(ToolSet::empty().with_read());

    // Assert
    assert!(!Arc::ptr_eq(&harness.inner, &reconfigured.inner));
    assert!(harness.inner.tools.resolve().is_empty());
    assert_eq!(
        reconfigured.inner.tools.resolve(),
        ToolSet::empty().with_read()
    );
    #[cfg(feature = "sqlite")]
    assert!(Arc::ptr_eq(
        &harness.inner.database,
        &reconfigured.inner.database
    ));
}

/// Window of the session model's profile in the summary catalogs.
const SESSION_WINDOW: u64 = 20_000;

/// Models of a summary catalog.
struct SummaryModels {
    catalog: Arc<ModelCatalog>,
    cheap: Arc<dyn Model>,
    review: Arc<dyn Model>,
    smart: Arc<dyn Model>,
}

/// Returns `host/cheap` with structured output under the declared profile.
fn structured_cheap_spec() -> ModelSpec {
    ModelSpec::new("host/cheap")
        .with_profile(declared_profile())
        .with_capabilities(Capabilities::NONE.with_structured_output(true))
}

/// Returns `host/smart` under a [`SESSION_WINDOW`] profile.
fn session_spec() -> ModelSpec {
    ModelSpec::new("host/smart").with_profile(ModelProfile::new(SESSION_WINDOW))
}

/// Returns a host model reporting `provider` in its metadata, when set,
/// that accepts every output schema unless `rejects_schemas` is set.
fn hosted_model(provider: Option<&'static str>, rejects_schemas: bool) -> Arc<dyn Model> {
    let mut model = MockModel::new();
    model.expect_metadata().return_const(
        provider.map(|provider| ModelMetadata::new(provider, "model").expect("metadata")),
    );
    model.expect_profile().return_const(None);
    model.expect_validate_input().returning(|_| Ok(()));
    model.expect_validate_schema().returning(move |_| {
        if rejects_schemas {
            return Err(ModelError::UnsupportedOutputSchema {
                reason: "no summaries".to_string(),
            });
        }

        Ok(())
    });

    Arc::new(model)
}

/// Returns a catalog assigning the smart role to the `host/smart` session
/// model and the cheap role to the `cheap` candidates, in order, each a
/// host model of the given metadata provider.
fn candidate_catalog(
    session_provider: Option<&'static str>,
    session_spec: ModelSpec,
    cheap: Vec<(ModelSpec, Option<&'static str>)>,
) -> Arc<ModelCatalog> {
    let ids: Vec<ModelId> = cheap.iter().map(|(spec, _)| spec.id().clone()).collect();
    let builder = cheap.into_iter().fold(
        ModelCatalog::builder().model(session_spec, hosted_model(session_provider, false)),
        |builder, (spec, provider)| builder.model(spec, hosted_model(provider, false)),
    );

    Arc::new(
        builder
            .assign(Role::SMART, ["host/smart"])
            .assign(Role::CHEAP, ids)
            .build()
            .expect("catalog should build"),
    )
}

/// Returns the identifier of the summary model `catalog` gives by default.
fn default_summary_id(catalog: &Arc<ModelCatalog>) -> Option<String> {
    Harness::builder()
        .catalog(Arc::clone(catalog))
        .build()
        .expect("cataloged model should build")
        .inner
        .summary_model
        .as_ref()
        .map(|summary| summary.id.as_str().to_string())
}

/// Returns a catalog assigning `host/smart` the smart role under a
/// [`SESSION_WINDOW`] profile, `host/cheap` described by `cheap_spec` the
/// cheap role, and `host/review`, a reasoning model with structured output
/// and without a profile, a host-defined review role, all from one host
/// provider.
fn summary_catalog(cheap_spec: ModelSpec) -> SummaryModels {
    let smart = hosted_model(Some("host"), false);
    let cheap = hosted_model(Some("host"), false);
    let review = hosted_model(Some("host"), false);
    let catalog = ModelCatalog::builder()
        .model(session_spec(), Arc::clone(&smart))
        .model(cheap_spec, Arc::clone(&cheap))
        .model(
            ModelSpec::new("host/review").with_capabilities(
                Capabilities::NONE
                    .with_reasoning(true)
                    .with_structured_output(true),
            ),
            Arc::clone(&review),
        )
        .assign(Role::SMART, ["host/smart"])
        .assign(Role::CHEAP, ["host/cheap"])
        .assign(Role::new("review"), ["host/review"])
        .build()
        .expect("catalog should build");

    SummaryModels {
        catalog: Arc::new(catalog),
        cheap,
        review,
        smart,
    }
}

/// Returns the configured summary model of `harness`.
fn summary_model(harness: &Harness) -> &SummaryModel {
    harness.inner.summary_model.as_ref().expect("summary model")
}

#[test]
fn build_resolves_the_cheap_role_as_the_summary_model_under_its_profile() {
    // Arrange
    let models = summary_catalog(structured_cheap_spec());

    // Act
    let harness = Harness::builder()
        .catalog(Arc::clone(&models.catalog))
        .build()
        .expect("cataloged model should build");

    // Assert
    let summary = summary_model(&harness);
    assert!(Arc::ptr_eq(&harness.inner.model, &models.smart));
    assert!(Arc::ptr_eq(&summary.model, &models.cheap));
    assert_eq!(summary.id.as_str(), "host/cheap");
    assert_eq!(
        summary.context_budget,
        declared_profile().context_budget().expect("valid profile")
    );
    assert_eq!(
        summary
            .context_estimator
            .request_weight(Some(&object_schema())),
        PROFILE_REQUEST_TOKENS
    );
    assert_eq!(summary.reasoning_effort, None);
}

#[test]
fn explicit_budgets_size_the_summary_model_up_to_its_declared_budget() {
    // Arrange
    let models = summary_catalog(structured_cheap_spec());
    let build = |budget: u64| {
        Harness::builder()
            .catalog(Arc::clone(&models.catalog))
            .context_budget(ContextBudget::new(
                NonZeroU64::new(budget).expect("non-zero"),
            ))
            .context_estimator(HeuristicContextEstimator::DEFAULT)
            .build()
            .expect("cataloged model should build")
    };

    // Act
    let lowered = build(500);
    let raised = build(1_000_000);

    // Assert
    let summary = summary_model(&lowered);
    assert_eq!(
        summary.context_budget,
        ContextBudget::new(NonZeroU64::new(500).expect("non-zero"))
    );
    assert!(Arc::ptr_eq(
        &summary.context_estimator,
        &lowered.inner.context_estimator
    ));
    assert_eq!(
        summary_model(&raised).context_budget,
        declared_profile().context_budget().expect("valid profile"),
        "an explicit budget never raises the summary model past its window"
    );
}

#[test]
fn explicit_summary_selector_resolves_a_model_without_a_profile_under_the_session_budget() {
    // Arrange
    let models = summary_catalog(structured_cheap_spec());

    // Act
    let harness = Harness::builder()
        .catalog(Arc::clone(&models.catalog))
        .summary_model_selector(Role::new("review"))
        .build()
        .expect("cataloged model should build");

    // Assert
    let summary = summary_model(&harness);
    assert!(Arc::ptr_eq(&summary.model, &models.review));
    assert_eq!(
        summary.context_budget,
        ModelProfile::new(SESSION_WINDOW)
            .context_budget()
            .expect("valid profile")
    );
    assert!(Arc::ptr_eq(
        &summary.context_estimator,
        &harness.inner.context_estimator
    ));
    assert_eq!(summary.reasoning_effort, Some(ReasoningEffort::Low));
}

#[test]
fn replacing_the_estimator_also_resizes_the_summary_model() {
    // Arrange
    let models = summary_catalog(structured_cheap_spec());
    let harness = Harness::builder()
        .catalog(Arc::clone(&models.catalog))
        .build()
        .expect("cataloged model should build");
    summary_model(&harness)
        .context_calibration
        .observe_overflow(1);

    // Act
    let harness = harness.context_estimator(HeuristicContextEstimator::DEFAULT);

    // Assert
    let summary = summary_model(&harness);
    assert!(Arc::ptr_eq(
        &summary.context_estimator,
        &harness.inner.context_estimator
    ));
    assert_eq!(
        summary
            .context_calibration
            .effective_budget(summary.context_budget)
            .1,
        Calibration::default(),
        "the previous estimator's calibration is discarded"
    );
}

#[test]
fn default_summaries_stay_on_the_session_model_without_a_capable_same_provider_cheap_model() {
    // Arrange
    let smart: Arc<dyn Model> = Arc::new(profiled_model(None));
    let smart_only = Arc::new(
        ModelCatalog::builder()
            .model(
                ModelSpec::new("host/smart").with_profile(ModelProfile::new(SESSION_WINDOW)),
                Arc::clone(&smart),
            )
            .assign(Role::SMART, ["host/smart"])
            .build()
            .expect("catalog should build"),
    );
    let unstructured = summary_catalog(ModelSpec::new("host/cheap"));
    let structured = summary_catalog(structured_cheap_spec());

    // Act
    let build = |catalog: &Arc<ModelCatalog>| {
        Harness::builder()
            .catalog(Arc::clone(catalog))
            .build()
            .expect("cataloged model should build")
    };
    let unassigned = build(&smart_only);
    let missing_capability = build(&unstructured.catalog);
    let same_model = Harness::builder()
        .catalog(Arc::clone(&structured.catalog))
        .model_selector(Role::CHEAP)
        .build()
        .expect("cheap model should build");
    let explicit_model = Harness::builder()
        .model(Arc::clone(&smart))
        .catalog(Arc::clone(&structured.catalog))
        .context_budget(unbounded_context_budget())
        .build()
        .expect("explicit model should build");

    // Assert
    assert!(unassigned.inner.summary_model.is_none());
    assert!(missing_capability.inner.summary_model.is_none());
    assert!(Arc::ptr_eq(&same_model.inner.model, &structured.cheap));
    assert!(same_model.inner.summary_model.is_none());
    assert!(explicit_model.inner.summary_model.is_none());
    assert!(explicit_model.inner.catalog.is_none());
}

#[test]
fn default_summary_models_are_the_first_capable_same_provider_cheap_candidate() {
    // Arrange
    let structured = |id: &str| {
        ModelSpec::new(id).with_capabilities(Capabilities::NONE.with_structured_output(true))
    };
    let pricing = Pricing::new(1_000_000, 0);
    let ordered = candidate_catalog(
        Some("host"),
        session_spec(),
        vec![
            (structured("other/cheap"), Some("other")),
            (ModelSpec::new("host/unstructured"), Some("host")),
            (structured("host/cheap"), Some("host")),
        ],
    );
    let other_vendor = candidate_catalog(
        Some("host"),
        session_spec(),
        vec![(structured("other/cheap"), Some("other"))],
    );
    let without_metadata =
        candidate_catalog(None, session_spec(), vec![(structured("host/cheap"), None)]);
    let priced = candidate_catalog(
        Some("host"),
        session_spec().with_pricing(pricing),
        vec![
            (structured("host/unpriced"), Some("host")),
            (
                structured("host/priced").with_pricing(pricing),
                Some("host"),
            ),
        ],
    );
    let rejecting = Arc::new(
        ModelCatalog::builder()
            .model(session_spec(), hosted_model(Some("host"), false))
            .model(structured("host/cheap"), hosted_model(Some("host"), true))
            .assign(Role::SMART, ["host/smart"])
            .assign(Role::CHEAP, ["host/cheap"])
            .build()
            .expect("catalog should build"),
    );

    // Act
    let selected = [
        default_summary_id(&ordered),
        default_summary_id(&other_vendor),
        default_summary_id(&without_metadata),
        default_summary_id(&priced),
        default_summary_id(&rejecting),
    ];

    // Assert
    assert_eq!(
        selected,
        [
            Some("host/cheap".to_string()),
            None,
            None,
            Some("host/priced".to_string()),
            None,
        ]
    );
}

#[test]
fn build_rejects_summary_selectors_of_models_that_cannot_summarize() {
    // Arrange
    let unstructured = summary_catalog(ModelSpec::new("host/cheap"));
    let rejecting = Arc::new(
        ModelCatalog::builder()
            .model(session_spec(), hosted_model(Some("host"), false))
            .model(structured_cheap_spec(), hosted_model(Some("host"), true))
            .assign(Role::SMART, ["host/smart"])
            .assign(Role::CHEAP, ["host/cheap"])
            .build()
            .expect("catalog should build"),
    );
    let failure = |catalog: &Arc<ModelCatalog>| {
        build_error(
            Harness::builder()
                .catalog(Arc::clone(catalog))
                .summary_model_selector(Role::CHEAP)
                .build(),
        )
    };

    // Act
    let errors = [failure(&unstructured.catalog), failure(&rejecting)];

    // Assert
    for error in &errors {
        assert!(matches!(
            error.get_ref().downcast_ref::<HarnessBuildError>(),
            Some(HarnessBuildError::SummaryModelUnsupported { id }) if id.as_str() == "host/cheap"
        ));
    }
    assert_eq!(
        errors[0].get_ref().to_string(),
        "summary model `host/cheap` cannot write summaries"
    );
}

#[test]
fn compaction_selectors_reuse_the_configured_summary_model_calibration() {
    // Arrange
    let models = summary_catalog(structured_cheap_spec());
    let harness = Harness::builder()
        .catalog(Arc::clone(&models.catalog))
        .build()
        .expect("cataloged model should build");
    let config = &harness.inner;
    let resolve = |selector: Role| {
        config
            .resolve_summary_model(&selector.into())
            .expect("cataloged selector")
    };

    // Act
    let configured = resolve(Role::CHEAP).expect("configured summary model");
    let other = resolve(Role::new("review")).expect("review summary model");
    let session = resolve(Role::SMART);
    configured.context_calibration.observe_overflow(1);

    // Assert
    let learned = |summary: &SummaryModel| {
        summary
            .context_calibration
            .effective_budget(summary.context_budget)
            .1
            != Calibration::default()
    };
    assert!(
        learned(summary_model(&harness)),
        "a ceiling learned through a selector applies to compact()"
    );
    assert!(!learned(&other));
    assert!(session.is_none());
}

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn only_explicit_summary_selectors_cross_providers() {
    // Arrange
    let models = summary_catalog(structured_cheap_spec().with_provider(ModelProvider::Kimi));

    // Act
    let default = Harness::builder()
        .catalog(Arc::clone(&models.catalog))
        .build()
        .expect("cataloged model should build");
    let explicit = Harness::builder()
        .catalog(Arc::clone(&models.catalog))
        .summary_model_selector(Role::CHEAP)
        .build()
        .expect("cataloged model should build");

    // Assert
    assert!(default.inner.summary_model.is_none());
    assert!(Arc::ptr_eq(&summary_model(&explicit).model, &models.cheap));
}

#[test]
fn build_reports_summary_selectors_it_cannot_resolve() {
    // Arrange
    let models = summary_catalog(ModelSpec::new("host/cheap"));
    let failure = |selector: ModelSelector| {
        build_error(
            Harness::builder()
                .catalog(Arc::clone(&models.catalog))
                .summary_model_selector(selector)
                .build(),
        )
    };

    // Act
    let with_model = build_error(
        Harness::builder()
            .model(Arc::clone(&models.smart))
            .catalog(Arc::clone(&models.catalog))
            .context_budget(unbounded_context_budget())
            .summary_model_selector(Role::CHEAP)
            .build(),
    );
    let unknown = failure(ModelId::new("host/missing").into());
    let unassigned = failure(Role::FAST.into());
    let incapable = failure(ModelSelector::Require {
        role: Role::CHEAP,
        needs: Capabilities::NONE.with_structured_output(true),
    });

    // Assert
    let message = |error: &HarnessError| {
        error
            .get_ref()
            .downcast_ref::<HarnessBuildError>()
            .map(ToString::to_string)
    };
    assert!(matches!(
        with_model.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::ConflictingModelSelection)
    ));
    assert!(matches!(
        unknown.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::Catalog(CatalogError::UnknownModel { id }))
            if id.as_str() == "host/missing"
    ));
    assert!(matches!(
        unassigned.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::Catalog(
            CatalogError::UnassignedRole { .. }
        ))
    ));
    assert!(matches!(
        incapable.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::Catalog(
            CatalogError::MissingCapability { .. }
        ))
    ));
    assert_eq!(
        message(&with_model).as_deref(),
        Some("set either `model` or a model selector, not both")
    );
}
