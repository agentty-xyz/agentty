//! Model registry contract exercised externally and in source coverage.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ag_harness::{
    ExecutionIdentity, Harness, Model, ModelCapabilities, ModelCompletion, ModelConfiguration,
    ModelError, ModelMetadata, ModelProvider, ModelRegistry, ModelRegistryError, ModelRequest,
    ModelResponse, SessionError, SqliteStore,
};
use async_trait::async_trait;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::store_conformance_test::{options, schema, stores};

struct CountingModel {
    calls: Arc<AtomicUsize>,
    name: &'static str,
}

#[async_trait]
impl Model for CountingModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(ModelMetadata::new("registry-test", self.name).expect("metadata"))
    }

    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);

        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({"answer": self.name}),
        )))
    }
}

struct AnonymousModel(CountingModel);

#[async_trait]
impl Model for AnonymousModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.0.complete(request).await
    }
}

fn registry(key: &str, revision: &str, calls: &Arc<AtomicUsize>) -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    registry
        .register(
            ExecutionIdentity::new(key, revision).expect("identity"),
            CountingModel {
                calls: Arc::clone(calls),
                name: "first",
            },
            ModelCapabilities::default(),
        )
        .expect("registration");

    registry
}

#[tokio::test]
async fn registry_selects_models_and_rejects_duplicates_without_replacement() {
    // Arrange
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = registry("primary", "1", &calls);
    let capabilities = ModelCapabilities {
        native_continuation: true,
        tool_calls: true,
    };
    registry
        .register(
            ExecutionIdentity::new("secondary", "2").expect("identity"),
            CountingModel {
                calls: Arc::clone(&calls),
                name: "second",
            },
            capabilities,
        )
        .expect("second registration");

    // Act
    for revision in ["1", "changed"] {
        let duplicate = registry.register(
            ExecutionIdentity::new("primary", revision).expect("identity"),
            CountingModel {
                calls: Arc::clone(&calls),
                name: "replacement",
            },
            capabilities,
        );
        let error = duplicate.expect_err("duplicate key");
        assert_eq!(
            error,
            ModelRegistryError::DuplicateKey {
                key: "primary".into()
            }
        );
        assert_eq!(
            error.to_string(),
            "model key `primary` is already registered"
        );
    }
    let first = Harness::from_registry(&registry, "primary").expect("primary");
    let second = Harness::from_registry(&registry, "secondary").expect("secondary");
    for key in ["missing", "", "Primary"] {
        let expected = ModelRegistryError::UnknownKey { key: key.into() };
        assert_eq!(expected.to_string(), format!("unknown model key `{key}`"));
        assert_eq!(registry.resolve(key).err(), Some(expected.clone()));
        assert_eq!(Harness::from_registry(&registry, key).err(), Some(expected));
    }
    drop(registry);
    let first_result = first.run_once("hello", schema()).await.expect("first");
    let second_result = second.run_once("hello", schema()).await.expect("second");

    // Assert
    assert_eq!(first_result.output(), &json!({"answer":"first"}));
    assert_eq!(second_result.output(), &json!({"answer":"second"}));
    let registration = second.model_registration().expect("captured registration");
    assert_eq!(registration.identity().key(), "secondary");
    assert_eq!(registration.identity().revision(), "2");
    assert_eq!(registration.capabilities(), capabilities);
    assert_eq!(registration.metadata().expect("metadata").model(), "second");
    assert_eq!(
        first
            .model_registration()
            .expect("registration")
            .capabilities(),
        ModelCapabilities::default()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn registered_identity_survives_snapshots_and_conflicts_on_changes() {
    for store in stores().await {
        // Arrange
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = registry("primary", "1", &calls);
        let harness = Harness::from_registry(&registry, "primary")
            .expect("harness")
            .store(Arc::clone(&store));
        let builder = harness.session("session", schema());
        drop(harness);
        drop(registry);
        let mut session = tokio::spawn(builder.create())
            .await
            .expect("task")
            .expect("session");

        // Act
        let original = session
            .submit("id", "hello", options())
            .await
            .expect("original");
        drop(session);
        let unchanged = self::registry("primary", "1", &calls);
        let harness = Harness::from_registry(&unchanged, "primary")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut resumed = harness.resume("session").await.expect("resume");
        drop(harness);
        let duplicate = resumed
            .submit("id", "hello", options())
            .await
            .expect("duplicate");
        for (key, revision) in [("primary", "2"), ("other", "1")] {
            let changed = self::registry(key, revision, &calls);
            let harness = Harness::from_registry(&changed, key)
                .expect("changed harness")
                .store(Arc::clone(&store))
                .execution_identity(ExecutionIdentity::new("primary", "1").expect("identity"));
            let result = harness.resume("session").await;
            assert!(matches!(
                result,
                Err(SessionError::RegistrationMismatch { .. })
            ));
        }
        let mut changed = ModelRegistry::new();
        changed
            .register(
                ExecutionIdentity::new("primary", "1").expect("identity"),
                CountingModel {
                    calls: Arc::clone(&calls),
                    name: "first",
                },
                ModelCapabilities {
                    native_continuation: false,
                    tool_calls: true,
                },
            )
            .expect("registration");
        let result = Harness::from_registry(&changed, "primary")
            .expect("harness")
            .store(store)
            .resume("session")
            .await
            .expect("resume")
            .submit("id", "hello", options())
            .await;

        // Assert
        assert_eq!(original, duplicate);
        assert!(matches!(result, Err(SessionError::HostTurnConflict)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn registry_reconstruction_recovers_sqlite_request_after_reopen() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("registry.sqlite");
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = registry("primary", "1", &calls);
    let harness = Harness::from_registry(&registry, "primary")
        .expect("harness")
        .database(&path);
    let mut session = harness
        .session("session", schema())
        .create()
        .await
        .expect("session");
    let original = session
        .submit("id", "hello", options())
        .await
        .expect("original");
    drop(session);
    drop(harness);
    drop(registry);

    // Act
    let reopened = Arc::new(SqliteStore::open(&path).await.expect("reopen"));
    for (key, revision) in [("other", "1"), ("primary", "2")] {
        let changed = self::registry(key, revision, &calls);
        let result = Harness::from_registry(&changed, key)
            .expect("harness")
            .store(reopened.clone())
            .resume("session")
            .await;
        assert!(matches!(
            result,
            Err(SessionError::RegistrationMismatch { .. })
        ));
    }
    let registry = self::registry("primary", "1", &calls);
    let mut session = Harness::from_registry(&registry, "primary")
        .expect("harness")
        .store(reopened)
        .resume("session")
        .await
        .expect("resume");
    let recovered = session
        .submit_controlled("id", "hello", options())
        .expect("controlled")
        .await
        .expect("recovered");

    // Assert
    assert_eq!(original, recovered);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    session
        .send("next ordinary turn")
        .await
        .expect("ordinary send after reopen");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn registered_builtin_configurations_execute_through_the_selected_client() {
    for provider in ModelProvider::all() {
        // Arrange
        let server = MockServer::start().await;
        let model = provider.known_models()[0];
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({"model":model})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id":"response", "model":model,
                "choices":[{"finish_reason":"stop","message":{"content":"{\"answer\":\"fixture\"}"}}]
            })))
            .expect(1)
            .mount(&server).await;
        let client = ModelConfiguration::new(*provider, model)
            .base_url(server.uri())
            .client_from_environment(|_| Ok("fixture-key".into()))
            .expect("client");
        let metadata = client.metadata().clone();
        let mut registry = ModelRegistry::new();
        let capabilities = ModelCapabilities {
            native_continuation: false,
            tool_calls: true,
        };
        registry
            .register(
                ExecutionIdentity::new("configured", "1").expect("identity"),
                client,
                capabilities,
            )
            .expect("registration");

        // Act
        let harness = Harness::from_registry(&registry, "configured").expect("harness");
        let output = harness.run_once("hello", schema()).await.expect("output");

        // Assert
        let registration = registry.resolve("configured").expect("registration");
        assert_eq!(registration.metadata(), Some(metadata));
        assert_eq!(registration.capabilities(), capabilities);
        assert_eq!(output.output(), &json!({"answer":"fixture"}));
        server.verify().await;
    }
}

#[tokio::test]
async fn direct_construction_keeps_optional_identity_and_executes_unchanged() {
    // Arrange
    let calls = Arc::new(AtomicUsize::new(0));
    let harness = Harness::new(CountingModel {
        calls: Arc::clone(&calls),
        name: "direct",
    });

    // Act
    let outcome = harness
        .run_once("hello", schema())
        .await
        .expect("direct output");

    // Assert
    assert!(harness.model_registration().is_none());
    assert_eq!(outcome.output(), &json!({"answer":"direct"}));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shared_and_boxed_models_register_and_retain_the_original_instance() {
    // Arrange
    let calls = Arc::new(AtomicUsize::new(0));
    let shared: Arc<dyn Model> = Arc::new(CountingModel {
        calls: Arc::clone(&calls),
        name: "shared",
    });
    let boxed: Box<dyn Model> = Box::new(CountingModel {
        calls: Arc::clone(&calls),
        name: "boxed",
    });
    let mut registry = ModelRegistry::new();

    // Act
    registry
        .register_shared(
            ExecutionIdentity::new("shared", "1").expect("identity"),
            Arc::clone(&shared),
            ModelCapabilities::default(),
        )
        .expect("shared registration");
    registry
        .register_shared(
            ExecutionIdentity::new("boxed", "1").expect("identity"),
            Arc::from(boxed),
            ModelCapabilities::default(),
        )
        .expect("boxed registration");
    let duplicate = registry.register_shared(
        ExecutionIdentity::new("shared", "2").expect("identity"),
        Arc::clone(&shared),
        ModelCapabilities::default(),
    );
    let shared_harness = Harness::from_registry(&registry, "shared").expect("harness");
    let boxed_harness = Harness::from_registry(&registry, "boxed").expect("harness");
    drop(registry);
    let first = shared_harness
        .run_once("hello", schema())
        .await
        .expect("shared output");
    let second = boxed_harness
        .run_once("hello", schema())
        .await
        .expect("boxed output");

    // Assert
    assert!(matches!(
        duplicate,
        Err(ModelRegistryError::DuplicateKey { .. })
    ));
    assert_eq!(first.output(), &json!({"answer":"shared"}));
    assert_eq!(second.output(), &json!({"answer":"boxed"}));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        shared_harness
            .model_registration()
            .expect("registration")
            .metadata(),
        shared.metadata()
    );
}

#[tokio::test]
async fn durable_sessions_reject_other_registrations_even_without_model_metadata() {
    for store in stores().await {
        for anonymous in [false, true] {
            // Arrange
            let id = if anonymous { "anonymous" } else { "named" };
            let calls = Arc::new(AtomicUsize::new(0));
            let model = CountingModel {
                calls: Arc::clone(&calls),
                name: "same-model",
            };
            let shared: Arc<dyn Model> = if anonymous {
                Arc::new(AnonymousModel(model))
            } else {
                Arc::new(model)
            };
            let mut registry = ModelRegistry::new();
            registry
                .register_shared(
                    ExecutionIdentity::new("primary", "1").expect("identity"),
                    Arc::clone(&shared),
                    ModelCapabilities::default(),
                )
                .expect("registration");
            registry
                .register_shared(
                    ExecutionIdentity::new("other", "1").expect("identity"),
                    Arc::clone(&shared),
                    ModelCapabilities::default(),
                )
                .expect("registration");
            let harness = Harness::from_registry(&registry, "primary")
                .expect("harness")
                .store(Arc::clone(&store));
            let mut session = harness
                .session(id, schema())
                .create()
                .await
                .expect("session");
            session.send("first").await.expect("first turn");
            let before = store
                .load_session(id)
                .await
                .expect("original configuration");

            // Act
            let other = Harness::from_registry(&registry, "other")
                .expect("harness")
                .store(Arc::clone(&store));
            let mut revised = ModelRegistry::new();
            revised
                .register_shared(
                    ExecutionIdentity::new("primary", "2").expect("identity"),
                    Arc::clone(&shared),
                    ModelCapabilities::default(),
                )
                .expect("revised registration");
            let revised = Harness::from_registry(&revised, "primary")
                .expect("harness")
                .store(Arc::clone(&store));
            let direct = Harness::new(AnonymousModel(CountingModel {
                calls: Arc::clone(&calls),
                name: "direct",
            }))
            .store(Arc::clone(&store));
            for different in [other, revised, direct] {
                let error = different
                    .resume(id)
                    .await
                    .err()
                    .expect("registration mismatch");
                assert!(matches!(error, SessionError::RegistrationMismatch { .. }));
                assert!(error.to_string().contains("different model registration"));
            }
            let after = store
                .load_session(id)
                .await
                .expect("unchanged configuration");

            // Assert
            assert_eq!(after.registration_identity, before.registration_identity);
            assert_eq!(after.turns, before.turns);
            assert_eq!(after.provider_session_id, before.provider_session_id);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            harness
                .resume(id)
                .await
                .expect("matching resume")
                .send("second")
                .await
                .expect("ordinary turn");
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }
}

#[tokio::test]
async fn direct_sessions_cannot_be_silently_adopted_by_a_registration() {
    for store in stores().await {
        // Arrange
        let calls = Arc::new(AtomicUsize::new(0));
        let direct = Harness::new(AnonymousModel(CountingModel {
            calls: Arc::clone(&calls),
            name: "first",
        }))
        .store(Arc::clone(&store));
        direct
            .session("direct", schema())
            .create()
            .await
            .expect("session");
        let registry = registry("registered", "1", &calls);
        let registered = Harness::from_registry(&registry, "registered")
            .expect("harness")
            .store(store);

        // Act
        let result = registered.resume("direct").await;
        let mut resumed = direct.resume("direct").await.expect("direct resume");
        let output = resumed.send("hello").await.expect("direct execution");

        // Assert
        assert!(matches!(
            result,
            Err(SessionError::RegistrationMismatch { .. })
        ));
        assert_eq!(output.output(), &json!({"answer":"first"}));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
