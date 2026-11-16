//! `HarnessBuilder` unit tests for model selection, budget derivation,
//! validation failures, applied options, and shared handle configuration.

use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::support::{model, object_schema, response_without_metadata};
use crate::catalog::{CatalogError, ModelCatalog, ModelSpec, Role};
use crate::context::{
    ContextBudget, ContextBudgetError, ContextSummaryPolicy, HeuristicContextEstimator,
};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::error::{ErrorKind, HarnessError};
use crate::file_system::MockFileSystem;
use crate::harness::{Harness, HarnessBuildError};
use crate::lifecycle::LifecycleEvent;
use crate::memory_store::MemoryStore;
use crate::model::{MockModel, Model, ModelProfile, ModelResponse, ReasoningEffort};
use crate::policy::ToolPolicy;
use crate::recovery::ExecutionIdentity;
use crate::repository::Repository;
use crate::store::SessionStore;
use crate::tool::Tool;

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
    let builders = [Tool::Bash, Tool::Read, Tool::Write].map(|tool| {
        Harness::builder()
            .model(Arc::new(profiled_model(Some(declared_profile()))))
            .allow(tool)
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
            .request_weight(&object_schema()),
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
            .request_weight(&object_schema()),
        PROFILE_REQUEST_TOKENS
    );
}

#[test]
fn build_resolves_the_catalog_selector_under_the_cataloged_profile() {
    // Arrange
    let profile = declared_profile();
    let smart: Arc<dyn Model> = Arc::new(model());
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
            .request_weight(&object_schema()),
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
fn explicit_model_takes_precedence_over_the_catalog() {
    // Arrange
    let explicit: Arc<dyn Model> = Arc::new(profiled_model(Some(declared_profile())));
    let empty = ModelCatalog::builder().build().expect("empty catalog");

    // Act
    let harness = Harness::builder()
        .catalog(Arc::new(empty))
        .model_selector(Role::VISION)
        .model(Arc::clone(&explicit))
        .build()
        .expect("explicit model should build");

    // Assert
    assert!(Arc::ptr_eq(&harness.inner.model, &explicit));
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

    // Act
    let harness = Harness::builder()
        .model(Arc::new(profiled_model(Some(declared_profile()))))
        .store(Arc::new(MemoryStore::new()))
        .sqlite("sessions.db")
        .repository(Repository::fixture("root"))
        .allow(Tool::Read)
        .allow(Tool::Write)
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
    assert_eq!(config.database_path, Some(PathBuf::from("sessions.db")));
    assert!(config.store.is_none());
    assert_eq!(config.repository, Some(Repository::fixture("root")));
    assert_eq!(
        config.policy,
        ToolPolicy::default().allow(Tool::Read).allow(Tool::Write)
    );
    assert_eq!(
        config
            .file_system
            .canonicalize(Path::new("any"))
            .await
            .expect("configured filesystem"),
        PathBuf::from(SENTINEL_PATH)
    );
    assert_eq!(config.max_history_bytes, 64);
    assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(config.context_estimator.request_weight(&object_schema()), 5);
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
    let reconfigured = shared.allow(Tool::Read);

    // Assert
    assert!(!Arc::ptr_eq(&harness.inner, &reconfigured.inner));
    assert_eq!(harness.inner.policy, ToolPolicy::default());
    assert_eq!(
        reconfigured.inner.policy,
        ToolPolicy::default().allow(Tool::Read)
    );
    assert!(Arc::ptr_eq(
        &harness.inner.database,
        &reconfigured.inner.database
    ));
}
