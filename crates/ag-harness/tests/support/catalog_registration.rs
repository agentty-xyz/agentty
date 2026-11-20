//! Catalog entries registered as durable models: sessions recorded on
//! today's manual `provider/model` registrations resume and replay on
//! catalog registrations and the reverse, harnesses built from a catalog
//! record the same registration, and switches keep the entry's prices.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ag_harness::catalog::{Capabilities, ModelId, ModelSpec, Pricing};
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ModelCapabilities, ModelCompletion, ModelMetadata,
    ModelProfile, ModelRegistry, ModelRegistryError, ModelRequest, ModelResponse,
};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::SessionStore;
use ag_harness::{
    ErrorKind, Harness, HarnessBuildError, HarnessError, Model, ModelCatalog, ModelError,
    ModelSelector, RunOutcome, RunOutput, RunRequest, SessionError,
};
use async_trait::async_trait;
use serde_json::json;

use crate::model_profile_fixture::unbounded_model_profile;
use crate::store_fixture::{options, schema, stores};

/// Registry key in the `provider/model` format hosts such as Agentty record.
const KEY: &str = "kimi/kimi-k3";
/// Registration revision Agentty records with its harness sessions.
const REVISION: &str = "agentty-native-2";

/// Answers `{"answer": <model>}`, counting calls and reporting 1,000 input
/// tokens per response.
struct ProviderModel {
    calls: Arc<AtomicUsize>,
    model: &'static str,
}

#[async_trait]
impl Model for ProviderModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(ModelMetadata::new("kimi", self.model).expect("metadata"))
    }

    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let usage = CompletionUsage::new(None, None, Some(1_000), Some(0), None, None);

        Ok(ModelCompletion::new(
            CompletionMetadata::new("stop".into(), None, None, None, Some(usage)),
            ModelResponse::Output(json!({"answer": self.model})),
        ))
    }
}

fn model(calls: &Arc<AtomicUsize>, model: &'static str) -> Arc<dyn Model> {
    Arc::new(ProviderModel {
        calls: Arc::clone(calls),
        model,
    })
}

/// Returns the capabilities a host declares for [`KEY`] by hand.
fn manual_capabilities() -> ModelCapabilities {
    ModelCapabilities {
        tool_calls: true,
        ..ModelCapabilities::new(unbounded_model_profile())
    }
}

/// Returns a catalog spec of `id` declaring what [`manual_capabilities`]
/// declares.
fn spec(id: &str) -> ModelSpec {
    ModelSpec::new(id)
        .with_capabilities(Capabilities::NONE.with_tool_calls(true))
        .with_profile(unbounded_model_profile())
}

/// Returns a catalog of [`KEY`] and the priced `kimi/priced`.
fn catalog(calls: &Arc<AtomicUsize>) -> Result<Arc<ModelCatalog>, Box<dyn Error>> {
    Ok(Arc::new(
        ModelCatalog::builder()
            .model(spec(KEY), model(calls, "kimi-k3"))
            .model(
                spec("kimi/priced").with_pricing(Pricing::new(1_000_000, 0)),
                model(calls, "priced"),
            )
            .build()?,
    ))
}

/// Registers every entry of `catalog` under `revision`.
fn catalog_registry(
    catalog: &ModelCatalog,
    revision: &str,
) -> Result<ModelRegistry, Box<dyn Error>> {
    let mut registry = ModelRegistry::new();
    for spec in catalog.specs() {
        let handle = catalog.resolve(&ModelSelector::Id(spec.id().clone()))?;
        registry.register_catalog_entry(&handle, revision)?;
    }

    Ok(registry)
}

/// Returns a harness built from `catalog` that selects `id` and records
/// its registration under `revision`.
fn catalog_harness(
    catalog: &Arc<ModelCatalog>,
    id: &str,
    revision: &str,
) -> Result<Harness, HarnessError> {
    Harness::builder()
        .catalog(Arc::clone(catalog))
        .model_selector(ModelId::from(id))
        .registration_revision(revision)
        .build()
}

fn build_error(result: Result<Harness, HarnessError>) -> HarnessError {
    let error = result.err().expect("build should fail");
    assert_eq!(error.kind(), ErrorKind::Config);

    error
}

#[tokio::test]
async fn sessions_stored_on_manual_registrations_resume_and_replay_on_catalog_registrations()
-> Result<(), Box<dyn Error>> {
    for store in stores().await {
        // Arrange
        let calls = Arc::new(AtomicUsize::new(0));
        let mut manual = ModelRegistry::new();
        manual.register_shared(
            ExecutionIdentity::new(KEY, REVISION)?,
            model(&calls, "kimi-k3"),
            manual_capabilities(),
        )?;
        let mut stored = Harness::from_registry(&manual, KEY)?
            .store(Arc::clone(&store))
            .session("stored", schema())
            .create()
            .await?;
        stored
            .turn("hello")
            .options(options())
            .host_id("first")
            .await?;
        drop(stored);
        let catalog = catalog(&calls)?;
        let registry = catalog_registry(&catalog, REVISION)?;

        // Act
        let mut outputs = Vec::new();
        for harness in [
            Harness::from_registry(&registry, KEY)?,
            catalog_harness(&catalog, KEY, REVISION)?,
        ] {
            let mut session = harness.store(Arc::clone(&store)).resume("stored").await?;
            let replayed = session
                .turn("hello")
                .options(options())
                .host_id("first")
                .await?;
            let next = session.turn("next").options(options()).await?;
            outputs.push((replayed.output().clone(), next.output().clone()));
        }

        // Assert
        let manual = manual.resolve(KEY)?;
        let registration = registry.resolve(KEY)?;
        assert_eq!(registration.identity(), manual.identity());
        assert_eq!(registration.capabilities(), manual.capabilities());
        assert_eq!(registration.metadata(), manual.metadata());
        assert_eq!(
            registration.catalog_spec().map(ModelSpec::id),
            Some(&ModelId::from(KEY))
        );
        assert!(manual.catalog_spec().is_none());
        let answer = json!({"answer": "kimi-k3"});
        assert_eq!(outputs, vec![(answer.clone(), answer); 2]);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "the retried request replays its recorded answer on both harnesses"
        );
    }

    Ok(())
}

#[tokio::test]
async fn sessions_created_from_a_catalog_resume_on_manual_registrations()
-> Result<(), Box<dyn Error>> {
    for store in stores().await {
        // Arrange
        let calls = Arc::new(AtomicUsize::new(0));
        let catalog = catalog(&calls)?;
        let harness = catalog_harness(&catalog, KEY, REVISION)?.store(Arc::clone(&store));
        let mut created = harness.session("created", schema()).create().await?;
        created.turn("hello").options(options()).await?;
        drop(created);
        let mut manual = ModelRegistry::new();
        manual.register_shared(
            ExecutionIdentity::new(KEY, REVISION)?,
            model(&calls, "kimi-k3"),
            manual_capabilities(),
        )?;
        let unregistered = Harness::builder()
            .catalog(Arc::clone(&catalog))
            .model_selector(ModelId::from(KEY))
            .build()?
            .store(Arc::clone(&store));
        let revised = catalog_harness(&catalog, KEY, "agentty-native-3")?.store(Arc::clone(&store));

        // Act
        let resumed = Harness::from_registry(&manual, KEY)?
            .store(Arc::clone(&store))
            .resume("created")
            .await;
        let without_registration = unregistered.resume("created").await;
        let with_revised_registration = revised.resume("created").await;

        // Assert
        resumed.expect("the manual registration resumes the catalog session");
        let recorded = harness
            .model_registration()
            .map(|registration| registration.identity().clone());
        assert_eq!(recorded, Some(ExecutionIdentity::new(KEY, REVISION)?));
        assert!(matches!(
            without_registration,
            Err(SessionError::RegistrationMismatch { stored: Some(stored), .. })
                if Some(&stored) == recorded.as_ref()
        ));
        assert!(matches!(
            with_revised_registration,
            Err(SessionError::RegistrationMismatch { stored: Some(stored), .. })
                if Some(&stored) == recorded.as_ref()
        ));
    }

    Ok(())
}

#[tokio::test]
async fn catalog_registrations_switch_models_and_keep_entry_prices() -> Result<(), Box<dyn Error>> {
    // Arrange
    let calls = Arc::new(AtomicUsize::new(0));
    let catalog = catalog(&calls)?;
    let registry = catalog_registry(&catalog, "1")?;
    let mut manual = ModelRegistry::new();
    manual.register_shared(
        ExecutionIdentity::new("kimi/manual", "1")?,
        model(&calls, "manual"),
        manual_capabilities(),
    )?;
    let store: Arc<dyn SessionStore> = stores().await.remove(0);
    let mut session = catalog_harness(&catalog, KEY, "1")?
        .store(Arc::clone(&store))
        .session("switched", schema())
        .create()
        .await?;

    // Act
    let unpriced = session.run(RunRequest::new("first")).await;
    session.switch_model(&registry, "kimi/priced").await?;
    let switched = session.run(RunRequest::new("second")).await;
    let resumed = Harness::from_registry(&registry, "kimi/priced")?
        .store(Arc::clone(&store))
        .resume("switched")
        .await?
        .run(RunRequest::new("third"))
        .await;
    let mut rebuilt = catalog_harness(&catalog, "kimi/priced", "1")?
        .store(Arc::clone(&store))
        .resume("switched")
        .await?;
    rebuilt.switch_model(&manual, "kimi/manual").await?;
    let bare = rebuilt.run(RunRequest::new("fourth")).await;

    // Assert
    let micro_usd = |outcome: &RunOutcome| outcome.cost.map(|cost| cost.micro_usd);
    assert_eq!(
        unpriced.result.as_ref().ok(),
        Some(&RunOutput::Json(json!({"answer": "kimi-k3"})))
    );
    assert_eq!(micro_usd(&unpriced), None);
    assert_eq!(micro_usd(&switched), Some(1_000));
    assert_eq!(micro_usd(&resumed), Some(1_000));
    assert_eq!(
        bare.result.ok(),
        Some(RunOutput::Json(json!({"answer": "manual"}))),
        "a session resumed through the builder's registration switches on"
    );

    Ok(())
}

#[tokio::test]
async fn catalog_registrations_supply_the_execution_identity_unless_set()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let calls = Arc::new(AtomicUsize::new(0));
    let catalog = catalog(&calls)?;
    let store: Arc<dyn SessionStore> = stores().await.remove(0);
    let explicit = || {
        Harness::builder()
            .catalog(Arc::clone(&catalog))
            .model_selector(ModelId::from(KEY))
            .execution_identity(ExecutionIdentity::new("host", "7").expect("identity"))
            .registration_revision(REVISION)
            .build()
            .map(|harness| harness.store(Arc::clone(&store)))
    };
    let unregistered = Harness::builder()
        .catalog(Arc::clone(&catalog))
        .model_selector(ModelId::from(KEY))
        .build()?
        .store(Arc::clone(&store));
    let registered = catalog_harness(&catalog, KEY, REVISION)?.store(Arc::clone(&store));
    let mut recorded = explicit()?.session("explicit", schema()).create().await?;
    recorded
        .turn("hello")
        .options(options())
        .host_id("first")
        .await?;
    drop(recorded);

    // Act
    // The registration identities match, so both resume; only the explicit
    // execution identity reproduces the recorded request's fingerprint.
    let on_registration_identity = registered
        .resume("explicit")
        .await?
        .turn("hello")
        .options(options())
        .host_id("first")
        .await;
    let on_explicit_identity = explicit()?
        .resume("explicit")
        .await?
        .turn("hello")
        .options(options())
        .host_id("first")
        .await;
    let mut session = unregistered
        .session("unregistered", schema())
        .create()
        .await?;
    let without_identity = session
        .turn("hello")
        .options(options())
        .host_id("unregistered")
        .await;

    // Assert
    assert!(
        on_registration_identity.is_err(),
        "the registration identity must not replace the explicit one"
    );
    assert_eq!(
        on_explicit_identity?.output(),
        &json!({"answer": "kimi-k3"})
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "only the first request reached the model"
    );
    assert!(matches!(
        without_identity,
        Err(SessionError::ExecutionIdentityRequired)
    ));

    Ok(())
}

#[test]
fn catalog_registration_rejects_invalid_entries() -> Result<(), Box<dyn Error>> {
    // Arrange
    let calls = Arc::new(AtomicUsize::new(0));
    let unprofiled = Arc::new(
        ModelCatalog::builder()
            .model(
                ModelSpec::new("host/unprofiled"),
                model(&calls, "unprofiled"),
            )
            .model(spec(""), model(&calls, "unnamed"))
            .build()?,
    );
    let unprofiled_handle = unprofiled.resolve(&ModelSelector::Id("host/unprofiled".into()))?;
    let unnamed_handle = unprofiled.resolve(&ModelSelector::Id("".into()))?;
    let mut registry = ModelRegistry::new();

    // Act
    let without_profile = registry.register_catalog_entry(&unprofiled_handle, "1");
    let without_name = registry.register_catalog_entry(&unnamed_handle, "1");
    let blank_revision = registry.register_catalog_entry(&unprofiled_handle, " ");
    let catalog = catalog(&calls)?;
    let mut duplicated = catalog_registry(&catalog, "1")?;
    let handle = catalog.resolve(&ModelSelector::Id(KEY.into()))?;
    let duplicate = duplicated.register_catalog_entry(&handle, "2");
    let explicit_model = build_error(
        Harness::builder()
            .model(model(&calls, "explicit"))
            .context_budget(unbounded_model_profile().context_budget()?)
            .registration_revision("1")
            .build(),
    );
    let unprofiled_build = build_error(
        Harness::builder()
            .catalog(Arc::clone(&unprofiled))
            .model_selector(ModelId::from("host/unprofiled"))
            .context_budget(ModelProfile::new(1_000).context_budget()?)
            .registration_revision("1")
            .build(),
    );

    // Assert
    let missing_profile = ModelRegistryError::ProfileRequired {
        key: "host/unprofiled".into(),
    };
    assert_eq!(without_profile, Err(missing_profile.clone()));
    assert_eq!(
        missing_profile.to_string(),
        "model key `host/unprofiled` declares no context profile"
    );
    let unnamed = ModelRegistryError::InvalidIdentity {
        key: String::new(),
        revision: "1".into(),
    };
    assert_eq!(without_name, Err(unnamed.clone()));
    assert_eq!(
        unnamed.to_string(),
        "model key `` with revision `1` is not a valid execution identity"
    );
    assert_eq!(
        blank_revision,
        Err(ModelRegistryError::InvalidIdentity {
            key: "host/unprofiled".into(),
            revision: " ".into(),
        })
    );
    assert_eq!(
        duplicate,
        Err(ModelRegistryError::DuplicateKey { key: KEY.into() })
    );
    assert_eq!(
        duplicated.resolve(KEY)?.identity().revision(),
        "1",
        "a duplicate never replaces the registration"
    );
    assert!(matches!(
        explicit_model.get_ref().downcast_ref::<HarnessBuildError>(),
        Some(HarnessBuildError::RegistrationRequiresCatalog)
    ));
    assert_eq!(
        explicit_model.get_ref().to_string(),
        "a registration revision requires the model to be selected from a catalog; call `catalog` \
         without `model`"
    );
    let Some(HarnessBuildError::Registration(source)) = unprofiled_build
        .get_ref()
        .downcast_ref::<HarnessBuildError>()
    else {
        return Err("an unprofiled entry cannot be registered".into());
    };
    assert_eq!(source, &missing_profile);
    assert_eq!(
        unprofiled_build.get_ref().to_string(),
        "cannot register the selected catalog model"
    );

    Ok(())
}
