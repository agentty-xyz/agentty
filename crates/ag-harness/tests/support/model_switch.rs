//! Atomic switching contract shared by public integration and source coverage.

use std::sync::{Arc, Mutex};

use ag_harness::{
    ExecutionIdentity, Harness, Model, ModelCapabilities, ModelCompletion, ModelError,
    ModelMessage, ModelMetadata, ModelRegistry, ModelRequest, ModelResponse, NewSession,
    SessionError, SqliteStore, ToolCall, TurnInput,
};
use async_trait::async_trait;
use serde_json::json;

use crate::store_conformance_test::{Gate, image_input, options, schema, stores};

struct RecordingModel {
    name: &'static str,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[async_trait]
impl Model for RecordingModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(ModelMetadata::new(self.name, self.name).expect("metadata"))
    }

    fn validate_input(&self, input: &TurnInput) -> Result<(), ModelError> {
        if input.has_images() && self.name != "vision" {
            return Err(ModelError::UnsupportedImageInput {
                reason: "recording model reads text only".to_string(),
            });
        }

        Ok(())
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.requests.lock().expect("requests").push(request);
        Ok(
            ModelCompletion::from_response(ModelResponse::Output(json!({"answer": self.name})))
                .with_provider_session_id(format!("{}-continuation", self.name)),
        )
    }
}

fn registry(requests: &Arc<Mutex<Vec<ModelRequest>>>) -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    for name in ["a", "b", "claims-vision", "no-tools", "vision"] {
        registry
            .register(
                ExecutionIdentity::new(name, "1").expect("identity"),
                RecordingModel {
                    name,
                    requests: Arc::clone(requests),
                },
                ModelCapabilities {
                    context_budget: None,
                    image_input: name.ends_with("vision"),
                    native_continuation: true,
                    tool_calls: name != "no-tools",
                },
            )
            .expect("register");
    }
    registry
}

async fn assert_recorded_model(
    session: &ag_harness::Session,
    id: &str,
    key: &str,
    generation: i64,
) {
    let model = session
        .recover(id)
        .await
        .expect("recover")
        .expect("record")
        .model
        .expect("model snapshot");
    assert_eq!(model.generation, generation);
    assert_eq!(model.registration_identity.expect("identity").key(), key);
}

#[tokio::test]
async fn switches_fence_stale_handles_and_preserve_request_recovery() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let harness = Harness::from_registry(&registry, "a")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("switch", schema())
            .create()
            .await
            .expect("session");
        let mut stale = harness.resume("switch").await.expect("stale handle");
        let original = session
            .submit("first", "hello", options())
            .await
            .expect("first turn");

        // Act
        session
            .switch_model(&registry, "b")
            .await
            .expect("switch to b");
        let recorded = stale
            .submit("first", "hello", options())
            .await
            .expect("retry on stale handle");
        assert_eq!(recorded.output(), original.output());
        assert!(matches!(
            stale.send("wrong model").await,
            Err(SessionError::StaleModel { .. })
        ));
        assert!(matches!(
            stale.switch_model(&registry, "a").await,
            Err(SessionError::StaleModel { .. })
        ));
        assert!(matches!(
            session.submit("first", "hello", options()).await,
            Err(SessionError::HostTurnConflict)
        ));
        session
            .submit("second", "continue", options())
            .await
            .expect("b turn");
        session
            .switch_model(&registry, "a")
            .await
            .expect("switch back");
        assert!(matches!(
            stale.send("ABA").await,
            Err(SessionError::StaleModel { .. })
        ));
        session
            .submit("first", "hello", options())
            .await
            .expect("retry after ABA");
        let mut resumed = harness.resume("switch").await.expect("resume a");
        resumed
            .send_controlled("third", options())
            .await
            .expect("controlled a turn");

        // Assert
        let loaded = store.load_session("switch").await.expect("load");
        assert_eq!(loaded.model_generation, 2);
        assert_eq!(loaded.registration_identity.expect("identity").key(), "a");
        assert_recorded_model(&session, "first", "a", 0).await;
        assert_recorded_model(&session, "second", "b", 1).await;
        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|request| request.provider_session_id().is_none())
        );
        assert!(
            requests[1]
                .messages()
                .iter()
                .any(|message| matches!(message, ModelMessage::User(text) if text == "hello"))
        );
        assert!(
            requests[2]
                .messages()
                .iter()
                .any(|message| matches!(message, ModelMessage::User(text) if text == "continue"))
        );
    }
}

#[tokio::test]
async fn switch_rejections_preserve_selection_and_continuation() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let harness = Harness::from_registry(&registry, "a")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("switch", schema())
            .create()
            .await
            .expect("session");
        session.send("hello").await.expect("turn");
        let before = store.load_session("switch").await.expect("before");

        // Act
        assert!(matches!(
            session.switch_model(&registry, "missing").await,
            Err(SessionError::Registry(_))
        ));
        let acquired = store
            .begin_turn(
                Arc::clone(&store),
                "switch",
                &TurnInput::from("active"),
                &options(),
                0,
            )
            .await
            .expect("acquire");
        assert!(matches!(
            session.switch_model(&registry, "b").await,
            Err(SessionError::Busy { .. })
        ));
        store
            .complete_turn(
                acquired.owner(),
                &[ModelMessage::Assistant("done".into())],
                Some("a-continuation"),
            )
            .await
            .expect("complete");
        drop(acquired);

        // Assert
        let after = store.load_session("switch").await.expect("after");
        assert_eq!(after.recorded_model(), before.recorded_model());
        assert_eq!(after.provider_session_id, before.provider_session_id);
        session
            .switch_model(&registry, "b")
            .await
            .expect("idle switch");
        assert!(
            store
                .load_session("switch")
                .await
                .expect("switched")
                .provider_session_id
                .is_none()
        );
    }
}

#[tokio::test]
async fn switches_validate_canonical_tool_and_reasoning_history() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let call = ToolCall::from_json("read-1".into(), "read", r#"{"path":"a.txt"}"#, None)
            .expect("call");
        let reasoning_call = ToolCall::from_json(
            "read-2".into(),
            "read",
            r#"{"path":"a.txt"}"#,
            Some("private provider state".into()),
        )
        .expect("reasoning call");
        let histories = [
            vec![
                ModelMessage::AssistantToolCall(call.clone()),
                ModelMessage::ToolResult {
                    call_id: "read-1".into(),
                    content: "text".into(),
                    name: "read".into(),
                },
            ],
            vec![ModelMessage::AssistantReasoning {
                content: "ordinary answer".into(),
                reasoning_content: "provider state".into(),
            }],
            vec![ModelMessage::AssistantToolCall(reasoning_call.clone())],
            vec![ModelMessage::AssistantToolCalls(vec![reasoning_call])],
            vec![ModelMessage::AssistantToolCalls(vec![call])],
        ];
        for (index, messages) in histories.iter().enumerate() {
            let id = format!("history-{index}");
            let config = NewSession::new(id.clone(), schema()).with_registration_identity(Some(
                ExecutionIdentity::new("a", "1").expect("identity"),
            ));
            store
                .create_session(
                    &config,
                    Some(ModelMetadata::new("a", "a").expect("metadata")),
                    1,
                )
                .await
                .expect("create");
            let acquired = store
                .begin_turn(
                    Arc::clone(&store),
                    &id,
                    &TurnInput::from("original"),
                    &options(),
                    0,
                )
                .await
                .expect("acquire");
            store
                .complete_turn(acquired.owner(), messages, Some("old"))
                .await
                .expect("complete");
            drop(acquired);
            let harness = Harness::from_registry(&registry, "a")
                .expect("harness")
                .store(Arc::clone(&store));
            let mut session = harness.resume(&id).await.expect("resume");

            // Act
            let target = if index == 0 || index == 4 {
                "no-tools"
            } else {
                "b"
            };
            assert!(matches!(
                session.switch_model(&registry, target).await,
                Err(SessionError::UnsupportedModelHistory { .. })
            ));

            // Assert
            let loaded = store.load_session(&id).await.expect("unchanged");
            assert_eq!(loaded.model_generation, 0);
            assert_eq!(loaded.provider_session_id.as_deref(), Some("old"));
            assert!(
                loaded.turns.is_empty(),
                "validation must include budget-evicted history"
            );
            if index == 0 || index == 4 {
                session
                    .switch_model(&registry, "b")
                    .await
                    .expect("tool capable target");
            }
        }
        assert!(requests.lock().expect("requests").is_empty());
    }
}

#[tokio::test]
async fn switches_validate_image_history_against_target_capabilities() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let config = NewSession::new("images", schema())
            .with_registration_identity(Some(ExecutionIdentity::new("a", "1").expect("identity")));
        store
            .create_session(
                &config,
                Some(ModelMetadata::new("a", "a").expect("metadata")),
                1,
            )
            .await
            .expect("create");
        let acquired = store
            .begin_turn(
                Arc::clone(&store),
                "images",
                &image_input("look", b"payload", "closely"),
                &options(),
                0,
            )
            .await
            .expect("acquire");
        store
            .complete_turn(
                acquired.owner(),
                &[ModelMessage::Assistant("described".into())],
                Some("old"),
            )
            .await
            .expect("complete");
        drop(acquired);
        let harness = Harness::from_registry(&registry, "a")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness.resume("images").await.expect("resume");

        // Act
        let rejected = session.switch_model(&registry, "b").await;
        let mismatched = session.switch_model(&registry, "claims-vision").await;

        // Assert
        assert!(matches!(
            rejected,
            Err(SessionError::UnsupportedModelHistory { .. })
        ));
        assert!(
            matches!(
                mismatched,
                Err(SessionError::UnsupportedModelHistory { .. })
            ),
            "a declaration the adapter rejects must not admit image history"
        );
        let loaded = store.load_session("images").await.expect("unchanged");
        assert_eq!(loaded.model_generation, 0);
        assert_eq!(loaded.registration_identity.expect("identity").key(), "a");
        assert_eq!(loaded.provider_session_id.as_deref(), Some("old"));
        assert!(
            loaded.turns.is_empty(),
            "validation must include budget-evicted image history"
        );
        session
            .switch_model(&registry, "vision")
            .await
            .expect("image-capable target");
        assert_eq!(
            store
                .load_session("images")
                .await
                .expect("switched")
                .model_generation,
            1
        );
    }
}

#[tokio::test]
async fn switching_survives_sqlite_reopen() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("switch.sqlite");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let registry = registry(&requests);
    let harness = Harness::from_registry(&registry, "a")
        .expect("harness")
        .database(&path);
    let mut session = harness
        .session("switch", schema())
        .create()
        .await
        .expect("session");
    session
        .submit("first", "hello", options())
        .await
        .expect("turn");

    // Act
    session.switch_model(&registry, "b").await.expect("switch");
    drop(session);
    drop(harness);
    let store = Arc::new(SqliteStore::open(&path).await.expect("reopen"));
    let harness = Harness::from_registry(&registry, "b")
        .expect("harness")
        .store(store);
    let mut session = harness.resume("switch").await.expect("resume b");
    session.send("b").await.expect("b turn");
    session
        .switch_model(&registry, "a")
        .await
        .expect("switch back");
    drop(session);
    drop(harness);
    let harness = Harness::from_registry(&registry, "a")
        .expect("harness")
        .database(&path);
    let mut session = harness.resume("switch").await.expect("resume a");
    session
        .submit("first", "hello", options())
        .await
        .expect("recorded retry");
    session.send("a again").await.expect("a turn");

    // Assert
    assert_eq!(requests.lock().expect("requests").len(), 3);
    assert_eq!(
        session
            .recover("first")
            .await
            .expect("recover")
            .expect("record")
            .model
            .expect("model")
            .generation,
        0
    );
}

#[tokio::test]
async fn switch_cannot_overtake_an_unacknowledged_acquisition() {
    // Arrange
    for store in stores().await {
        for after_commit in [false, true] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let registry = registry(&requests);
            let gate = Arc::new(Gate::new(Arc::clone(&store), after_commit));
            let harness = Harness::from_registry(&registry, "a")
                .expect("harness")
                .store(gate.clone());
            let id = format!("acquire-{after_commit}");
            let mut first = harness
                .session(&id, schema())
                .create()
                .await
                .expect("session");
            let mut other = harness.resume(&id).await.expect("other handle");

            // Act
            let task = tokio::spawn(async move { first.send("first").await });
            gate.entered.notified().await;
            assert!(matches!(
                other.switch_model(&registry, "b").await,
                Err(SessionError::Busy { .. })
            ));
            gate.release.notify_one();
            task.await.expect("task").expect("turn");
            other
                .switch_model(&registry, "b")
                .await
                .expect("idle switch");

            // Assert
            assert_eq!(
                store
                    .load_session(&id)
                    .await
                    .expect("loaded")
                    .model_generation,
                1
            );
            assert_eq!(requests.lock().expect("requests").len(), 1);
        }
    }
}

#[tokio::test]
async fn dropped_switch_waiter_retains_admission_until_acknowledgment() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = Arc::new(registry(&requests));
        let mut gate = Gate::new(Arc::clone(&store), true);
        gate.pause_switch = true;
        let gate = Arc::new(gate);
        let harness = Harness::from_registry(&registry, "a")
            .expect("harness")
            .store(gate.clone());
        let mut session = harness
            .session("cancel-switch", schema())
            .create()
            .await
            .expect("session");
        let task = {
            let registry = Arc::clone(&registry);
            tokio::spawn(async move { session.switch_model(&registry, "b").await })
        };
        gate.entered.notified().await;

        // Act
        task.abort();
        assert!(task.await.expect_err("cancelled waiter").is_cancelled());
        let harness = Harness::from_registry(&registry, "b")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut resumed = harness
            .resume("cancel-switch")
            .await
            .expect("committed model");
        assert!(matches!(
            resumed.send("blocked").await,
            Err(SessionError::Busy { .. })
        ));
        gate.release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match resumed.send("after acknowledgment").await {
                    Err(SessionError::Busy { .. }) => tokio::task::yield_now().await,
                    result => {
                        result.expect("turn after settlement");
                        break;
                    }
                }
            }
        })
        .await
        .expect("settled switch");

        // Assert
        assert_eq!(requests.lock().expect("requests").len(), 1);
        assert_eq!(
            store
                .load_session("cancel-switch")
                .await
                .expect("load")
                .model_generation,
            1
        );
    }
}

#[tokio::test]
async fn independent_store_admission_checks_generation_atomically() {
    // Arrange
    for store in stores().await {
        let identity = ExecutionIdentity::new("b", "1").expect("identity");
        let capabilities = ModelCapabilities {
            context_budget: None,
            image_input: false,
            native_continuation: true,
            tool_calls: true,
        };
        for switch_first in [false, true] {
            let id = format!("race-{switch_first}");
            store
                .create_session(&NewSession::new(&id, schema()), None, 1024)
                .await
                .expect("session");
            let turn_options = options();
            let switch = store.switch_model(&id, 0, &identity, None, capabilities);
            let input = TurnInput::from("prompt");
            let acquire = store.begin_turn(Arc::clone(&store), &id, &input, &turn_options, 0);

            // Act
            let (switched, acquired) = if switch_first {
                tokio::join!(switch, acquire)
            } else {
                let (acquired, switched) = tokio::join!(acquire, switch);
                (switched, acquired)
            };

            // Assert
            match (switched, acquired) {
                (Ok(1), Err(SessionError::StaleModel { .. })) => {}
                (Err(SessionError::Busy { .. }), Ok(acquired)) => {
                    store
                        .complete_turn(acquired.owner(), &[], None)
                        .await
                        .expect("complete");
                }
                _ => std::panic::resume_unwind(Box::new(
                    "switch and acquisition were not mutually exclusive",
                )),
            }
        }
    }
}

#[tokio::test]
async fn switching_checks_adapter_schema_without_network_access() {
    // Arrange
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut registry = registry(&requests);
    let server = wiremock::MockServer::start().await;
    registry
        .register(
            ExecutionIdentity::new("builtin", "1").expect("identity"),
            ag_harness::ModelClient::muse(ag_harness::MuseConfig {
                api_key: "test".into(),
                base_url: server.uri(),
                model: "muse-spark-1.3".into(),
            })
            .expect("client"),
            ModelCapabilities {
                context_budget: None,
                image_input: false,
                native_continuation: false,
                tool_calls: true,
            },
        )
        .expect("register");
    for store in stores().await {
        let harness = Harness::from_registry(&registry, "a")
            .expect("harness")
            .store(Arc::clone(&store));
        let scalar = ag_harness::OutputSchema::new(json!({"type":"string"})).expect("schema");
        let mut session = harness
            .session("scalar", scalar)
            .create()
            .await
            .expect("session");

        // Act
        let error = session
            .switch_model(&registry, "builtin")
            .await
            .expect_err("unsupported schema");

        // Assert
        assert!(matches!(
            error,
            SessionError::Turn(ag_harness::TurnError::Model(
                ModelError::UnsupportedOutputSchema { .. }
            ))
        ));
        assert_eq!(
            store
                .load_session("scalar")
                .await
                .expect("load")
                .model_generation,
            0
        );
        let mut session = harness
            .session("object", schema())
            .create()
            .await
            .expect("session");
        session
            .switch_model(&registry, "builtin")
            .await
            .expect("compatible schema");
    }
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn switching_preserves_tool_groups_and_explicit_execution_identity() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let harness = Harness::from_registry(&registry, "a")
            .expect("harness")
            .execution_identity(ExecutionIdentity::new("host-environment", "7").expect("identity"))
            .store(Arc::clone(&store));
        let mut session = harness
            .session("tool-history", schema())
            .create()
            .await
            .expect("session");
        let call =
            ToolCall::from_json("read".into(), "read", r#"{"path":"a.txt"}"#, None).expect("call");
        let messages = vec![
            ModelMessage::AssistantToolCalls(vec![call]),
            ModelMessage::ToolResult {
                call_id: "read".into(),
                content: "kept".into(),
                name: "read".into(),
            },
        ];
        let acquired = store
            .begin_turn(
                Arc::clone(&store),
                "tool-history",
                &TurnInput::from("original"),
                &options(),
                0,
            )
            .await
            .expect("acquire");
        store
            .complete_turn(acquired.owner(), &messages, Some("a-continuation"))
            .await
            .expect("complete");
        drop(acquired);
        session
            .submit("before", "hello", options())
            .await
            .expect("original request");

        // Act
        session
            .switch_model(&registry, "b")
            .await
            .expect("switch b");
        session.send("after switch").await.expect("b turn");
        session
            .switch_model(&registry, "a")
            .await
            .expect("switch back");
        session
            .submit("before", "hello", options())
            .await
            .expect("original fingerprint retains host override");

        // Assert
        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .messages()
                .windows(2)
                .any(|pair| pair == messages.as_slice())
        );
        assert!(requests[1].provider_session_id().is_none());
    }
}

#[tokio::test]
async fn switch_errors_do_not_admit_partial_selection() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let mut gate = Gate::new(Arc::clone(&store), false);
        gate.panic_switch = true;
        let harness = Harness::from_registry(&registry, "a")
            .expect("harness")
            .store(Arc::new(gate));
        let mut session = harness
            .session("switch", schema())
            .create()
            .await
            .expect("session");

        // Act / Assert
        assert!(matches!(
            store
                .switch_model(
                    "missing",
                    0,
                    &ExecutionIdentity::new("b", "1").expect("identity"),
                    None,
                    ModelCapabilities::default()
                )
                .await,
            Err(SessionError::NotFound { .. })
        ));
        assert!(matches!(
            session.switch_model(&registry, "b").await,
            Err(SessionError::Store {
                operation: "switch session model",
                ..
            })
        ));
        assert_eq!(
            store
                .load_session("switch")
                .await
                .expect("load")
                .model_generation,
            0
        );
        let harness = Harness::from_registry(&registry, "a")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness.resume("switch").await.expect("resume");
        session.send("admission released").await.expect("turn");
    }
}
