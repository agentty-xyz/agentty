//! Atomic switching contract shared by public integration and source coverage.

use std::sync::{Arc, Mutex};

use ag_harness::model::{
    ModelCapabilities, ModelCompletion, ModelMessage, ModelMetadata, ModelProfile, ModelRegistry,
    ModelRequest, ModelResponse,
};
use ag_harness::recovery::ExecutionIdentity;
#[cfg(feature = "sqlite")]
use ag_harness::store::SqliteStore;
use ag_harness::store::{AcquiredTurn, HistoryProjection, ModelSwitch, NewSession, SessionStore};
use ag_harness::tool::ToolCall;
use ag_harness::{Harness, Model, ModelError, SessionError, TurnInput};
use async_trait::async_trait;
use serde_json::json;

use crate::model_profile_fixture::unbounded_model_profile;
use crate::store_fixture::{image_input, options, schema, stores};
use crate::store_gate::Gate;

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
        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({"answer": self.name}),
        )))
    }
}

fn registry(requests: &Arc<Mutex<Vec<ModelRequest>>>) -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    for name in [
        "a",
        "b",
        "claims-vision",
        "kimi",
        "no-tools",
        "openai",
        "vision",
    ] {
        registry
            .register(
                ExecutionIdentity::new(name, "1").expect("identity"),
                RecordingModel {
                    name,
                    requests: Arc::clone(requests),
                },
                ModelCapabilities {
                    image_input: name.ends_with("vision"),
                    profile: unbounded_model_profile(),
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
            .turn("hello")
            .options(options())
            .host_id("first")
            .await
            .expect("first turn");

        // Act
        session
            .switch_model(&registry, "b")
            .await
            .expect("switch to b");
        let recorded = stale
            .turn("hello")
            .options(options())
            .host_id("first")
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
            session
                .turn("hello")
                .options(options())
                .host_id("first")
                .await,
            Err(SessionError::HostTurnConflict)
        ));
        session
            .turn("continue")
            .options(options())
            .host_id("second")
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
            .turn("hello")
            .options(options())
            .host_id("first")
            .await
            .expect("retry after ABA");
        let mut resumed = harness.resume("switch").await.expect("resume a");
        resumed
            .turn("third")
            .options(options())
            .start()
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
async fn switch_rejections_preserve_selection() {
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
        let acquired = AcquiredTurn::begin(
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
                HistoryProjection::default(),
            )
            .await
            .expect("complete");
        drop(acquired);

        // Assert
        let after = store.load_session("switch").await.expect("after");
        assert_eq!(after.recorded_model(), before.recorded_model());
        session
            .switch_model(&registry, "b")
            .await
            .expect("idle switch");
        let switched = store.load_session("switch").await.expect("switched");
        assert_eq!(switched.registration_identity.expect("identity").key(), "b");
    }
}

#[tokio::test]
async fn switches_reject_tool_history_for_tool_less_targets_but_accept_reasoning() {
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
            let acquired = AcquiredTurn::begin(
                Arc::clone(&store),
                &id,
                &TurnInput::from("original"),
                &options(),
                0,
            )
            .await
            .expect("acquire");
            store
                .complete_turn(acquired.owner(), messages, HistoryProjection::default())
                .await
                .expect("complete");
            drop(acquired);
            let harness = Harness::from_registry(&registry, "a")
                .expect("harness")
                .store(Arc::clone(&store));
            let mut session = harness.resume(&id).await.expect("resume");

            // Act
            let tool_less = session.switch_model(&registry, "no-tools").await;

            // Assert
            let loaded = store.load_session(&id).await.expect("loaded");
            assert!(
                loaded.turns.is_empty(),
                "validation must include budget-evicted history"
            );
            if index == 1 {
                tool_less.expect("reasoning alone replays as notes");
                assert_eq!(loaded.model_generation, 1);
            } else {
                assert!(matches!(
                    tool_less,
                    Err(SessionError::UnsupportedModelHistory {
                        reason: "target does not support tool history"
                    })
                ));
                assert_eq!(loaded.model_generation, 0);
                session
                    .switch_model(&registry, "b")
                    .await
                    .expect("tool capable target accepts reasoning tool history");
            }
        }
        assert!(requests.lock().expect("requests").is_empty());
    }
}

#[tokio::test]
async fn unsupported_history_precedes_stale_and_busy_switch_rejections() {
    // Arrange
    for store in stores().await {
        store
            .create_session(&NewSession::new("precedence", schema()), None, 1024)
            .await
            .expect("session");
        let completed = AcquiredTurn::begin(
            Arc::clone(&store),
            "precedence",
            &TurnInput::from("first"),
            &options(),
            0,
        )
        .await
        .expect("acquire");
        store
            .complete_turn(
                completed.owner(),
                &[
                    ModelMessage::AssistantToolCall(
                        ToolCall::from_json("read-1".into(), "read", r#"{"path":"a.txt"}"#, None)
                            .expect("call"),
                    ),
                    ModelMessage::ToolResult {
                        call_id: "read-1".into(),
                        content: "text".into(),
                        name: "read".into(),
                    },
                ],
                HistoryProjection::default(),
            )
            .await
            .expect("complete");
        drop(completed);
        let active = AcquiredTurn::begin(
            Arc::clone(&store),
            "precedence",
            &TurnInput::from("active"),
            &options(),
            0,
        )
        .await
        .expect("active");
        let switch = |generation| {
            ModelSwitch::new(
                ExecutionIdentity::new("b", "1").expect("identity"),
                None,
                ModelCapabilities::new(unbounded_model_profile()),
                generation,
            )
        };

        // Act
        let stale = store.switch_model("precedence", &switch(1)).await;
        let busy = store.switch_model("precedence", &switch(0)).await;

        // Assert
        assert!(matches!(
            stale,
            Err(SessionError::UnsupportedModelHistory { .. })
        ));
        assert!(matches!(
            busy,
            Err(SessionError::UnsupportedModelHistory { .. })
        ));
        store
            .complete_turn(active.owner(), &[], HistoryProjection::default())
            .await
            .expect("complete active");
        assert_eq!(
            store
                .load_session("precedence")
                .await
                .expect("load")
                .model_generation,
            0
        );
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
        let acquired = AcquiredTurn::begin(
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
                HistoryProjection::default(),
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

#[cfg(feature = "sqlite")]
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
        .turn("hello")
        .options(options())
        .host_id("first")
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
        .turn("hello")
        .options(options())
        .host_id("first")
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

/// Heading of a note carrying another provider's readable reasoning.
const CARRIED: &str = "[harness] Reasoning an earlier model wrote at this step, kept as a note:\n";

/// Note replacing another provider's unreadable reasoning.
const OMITTED: &str =
    "[harness] An earlier model reasoned at this step; that reasoning is not readable here.";

/// Encrypted `Responses` reasoning, summarized as `summary` when given.
fn responses_reasoning(summary: Option<&str>) -> String {
    let summary: Vec<_> = summary
        .map(|text| json!({"type": "summary_text", "text": text}))
        .into_iter()
        .collect();

    json!([{"type": "reasoning", "encrypted_content": "opaque", "summary": summary}]).to_string()
}

/// Returns the `read` call of `a.txt` with `reasoning` and `text`.
fn reasoning_call(reasoning: Option<String>, text: Option<&str>) -> ToolCall {
    ToolCall::from_json("read-1".into(), "read", r#"{"path":"a.txt"}"#, reasoning)
        .and_then(|call| call.with_assistant_text(text.map(str::to_string)))
        .expect("call")
}

/// Turn messages an `openai` model wrote: a tool call and a final answer,
/// each with encrypted reasoning.
fn openai_turn() -> Vec<ModelMessage> {
    vec![
        ModelMessage::AssistantToolCall(reasoning_call(
            Some(responses_reasoning(Some("Read a.txt."))),
            Some("Reading a.txt."),
        )),
        ModelMessage::ToolResult {
            call_id: "read-1".into(),
            content: "text".into(),
            name: "read".into(),
        },
        ModelMessage::AssistantReasoning {
            content: r#"{"answer":"seen"}"#.into(),
            reasoning_content: responses_reasoning(None),
        },
    ]
}

/// Creates session `id` selecting `openai` and completes one turn of
/// [`openai_turn`] for input `inspect` through `store`.
async fn create_openai_session(store: &Arc<dyn SessionStore>, id: &str) {
    let config = NewSession::new(id, schema()).with_registration_identity(Some(
        ExecutionIdentity::new("openai", "1").expect("identity"),
    ));
    store
        .create_session(
            &config,
            Some(ModelMetadata::new("openai", "openai").expect("metadata")),
            1 << 20,
        )
        .await
        .expect("create");
    complete_stored_turn(store, id, "inspect", 0, &openai_turn()).await;
}

/// Completes one turn of `input` answered by `messages` directly through
/// `store`, under model selection `generation`.
async fn complete_stored_turn(
    store: &Arc<dyn SessionStore>,
    id: &str,
    input: &str,
    generation: i64,
    messages: &[ModelMessage],
) {
    let acquired = AcquiredTurn::begin(
        Arc::clone(store),
        id,
        &TurnInput::from(input),
        &options(),
        generation,
    )
    .await
    .expect("acquire");
    store
        .complete_turn(acquired.owner(), messages, HistoryProjection::default())
        .await
        .expect("complete");
}

#[tokio::test]
async fn switches_replay_foreign_reasoning_as_notes_and_restore_it_natively() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        create_openai_session(&store, "project").await;
        let harness = Harness::from_registry(&registry, "openai")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness.resume("project").await.expect("resume");
        let kimi_answer = ModelMessage::AssistantReasoning {
            content: r#"{"answer":"thought"}"#.into(),
            reasoning_content: "Plain kimi reasoning.".into(),
        };

        // Act
        session
            .switch_model(&registry, "kimi")
            .await
            .expect("switch to kimi");
        complete_stored_turn(
            &store,
            "project",
            "think",
            1,
            std::slice::from_ref(&kimi_answer),
        )
        .await;
        session.send("next").await.expect("kimi turn");
        session
            .switch_model(&registry, "openai")
            .await
            .expect("switch back");
        session.send("back").await.expect("openai turn");

        // Assert
        let stored = store.load_session("project").await.expect("load");
        assert_eq!(stored.turns[0].1.messages[1..], openai_turn());
        assert_eq!(
            stored.turns[1].1.messages[1..],
            *std::slice::from_ref(&kimi_answer)
        );
        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].messages(),
            [
                ModelMessage::User("inspect".into()),
                ModelMessage::AssistantToolCall(reasoning_call(
                    None,
                    Some(&format!("{CARRIED}Read a.txt.\n\nReading a.txt.")),
                )),
                ModelMessage::ToolResult {
                    call_id: "read-1".into(),
                    content: "text".into(),
                    name: "read".into(),
                },
                ModelMessage::Assistant(OMITTED.into()),
                ModelMessage::Assistant(r#"{"answer":"seen"}"#.into()),
                ModelMessage::User("think".into()),
                kimi_answer,
                ModelMessage::User("next".into()),
            ]
        );
        let mut native = vec![ModelMessage::User("inspect".into())];
        native.extend(openai_turn());
        native.extend([
            ModelMessage::User("think".into()),
            ModelMessage::Assistant(format!("{CARRIED}Plain kimi reasoning.")),
            ModelMessage::Assistant(r#"{"answer":"thought"}"#.into()),
            ModelMessage::User("next".into()),
            ModelMessage::Assistant(r#"{"answer":"kimi"}"#.into()),
            ModelMessage::User("back".into()),
        ]);
        assert_eq!(requests[1].messages(), native);
    }
}

#[tokio::test]
async fn projected_history_survives_sqlite_reopen_after_a_switch() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("project.sqlite");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let registry = registry(&requests);
    let store: Arc<dyn SessionStore> = Arc::new(SqliteStore::open(&path).await.expect("open"));
    create_openai_session(&store, "project").await;
    let mut session = Harness::from_registry(&registry, "openai")
        .expect("harness")
        .store(Arc::clone(&store))
        .resume("project")
        .await
        .expect("resume");
    session
        .switch_model(&registry, "kimi")
        .await
        .expect("switch to kimi");
    drop(session);
    drop(store);

    // Act
    let mut resumed = Harness::from_registry(&registry, "kimi")
        .expect("harness")
        .database(&path)
        .resume("project")
        .await
        .expect("resume kimi");
    resumed.send("next").await.expect("kimi turn");
    resumed.send("again").await.expect("second kimi turn");

    // Assert
    let reopened = SqliteStore::open(&path).await.expect("reopen");
    let stored = reopened.load_session("project").await.expect("load");
    assert_eq!(stored.turns[0].1.messages[1..], openai_turn());
    let requests = requests.lock().expect("requests");
    assert_eq!(
        requests[1].messages()[..6],
        requests[0].messages()[..6],
        "the projected prefix stays stable for the provider cache"
    );
    assert_eq!(
        requests[0].messages()[1..5],
        [
            ModelMessage::AssistantToolCall(reasoning_call(
                None,
                Some(&format!("{CARRIED}Read a.txt.\n\nReading a.txt.")),
            )),
            ModelMessage::ToolResult {
                call_id: "read-1".into(),
                content: "text".into(),
                name: "read".into(),
            },
            ModelMessage::Assistant(OMITTED.into()),
            ModelMessage::Assistant(r#"{"answer":"seen"}"#.into()),
        ]
    );
}

/// Registers a large-window `a` and a small-window `b`, each recording its
/// requests.
fn windowed_registry(requests: &Arc<Mutex<Vec<ModelRequest>>>) -> ModelRegistry {
    let small_profile = ModelProfile::new(1_200);
    let mut registry = ModelRegistry::new();
    for (name, profile) in [("a", unbounded_model_profile()), ("b", small_profile)] {
        registry
            .register(
                ExecutionIdentity::new(name, "1").expect("identity"),
                RecordingModel {
                    name,
                    requests: Arc::clone(requests),
                },
                ModelCapabilities::new(profile),
            )
            .expect("register");
    }
    registry
}

#[tokio::test]
async fn switching_to_a_smaller_window_fits_history_to_the_new_budget() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = windowed_registry(&requests);
        let harness = Harness::from_registry(&registry, "a")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("shrink", schema())
            .create()
            .await
            .expect("session");
        for turn in 0..3 {
            session
                .send(format!("turn {turn}: {}", "history ".repeat(200)))
                .await
                .expect("large-window turn");
        }
        let large = session.send("before switch").await.expect("large turn");

        // Act
        session
            .switch_model(&registry, "b")
            .await
            .expect("switch to the smaller window");
        let small = session.send("after switch").await.expect("small turn");
        session
            .switch_model(&registry, "a")
            .await
            .expect("switch back to the larger window");
        let restored = session.send("after return").await.expect("large turn");

        // Assert
        assert_eq!(large.report().history().replayed_turns(), 3);
        assert_eq!(large.report().history().evicted_turns(), 0);
        assert!(small.report().history().evicted_turns() > 0);
        assert_eq!(
            small.report().history().replayed_turns() + small.report().history().evicted_turns(),
            4
        );
        assert_eq!(restored.report().history().replayed_turns(), 5);
        assert_eq!(restored.report().history().evicted_turns(), 0);
        let requests = requests.lock().expect("requests");
        assert!(requests[4].messages().len() < requests[5].messages().len());
    }
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
            image_input: false,
            profile: unbounded_model_profile(),
            tool_calls: true,
        };
        for switch_first in [false, true] {
            let id = format!("race-{switch_first}");
            store
                .create_session(&NewSession::new(&id, schema()), None, 1024)
                .await
                .expect("session");
            let turn_options = options();
            let selection = ModelSwitch::new(identity.clone(), None, capabilities, 0);
            let switch = store.switch_model(&id, &selection);
            let input = TurnInput::from("prompt");
            let acquire = AcquiredTurn::begin(Arc::clone(&store), &id, &input, &turn_options, 0);

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
                        .complete_turn(acquired.owner(), &[], HistoryProjection::default())
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

#[cfg(feature = "providers-compat")]
#[tokio::test]
async fn switching_checks_adapter_schema_without_network_access() {
    // Arrange
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut registry = registry(&requests);
    let server = wiremock::MockServer::start().await;
    registry
        .register(
            ExecutionIdentity::new("builtin", "1").expect("identity"),
            ag_harness::model::ModelClient::muse(ag_harness::provider::MuseConfig {
                api_key: "test".into(),
                base_url: server.uri(),
                model: "muse-spark-1.3".into(),
            })
            .expect("client"),
            ModelCapabilities {
                image_input: false,
                profile: unbounded_model_profile(),
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
        let acquired = AcquiredTurn::begin(
            Arc::clone(&store),
            "tool-history",
            &TurnInput::from("original"),
            &options(),
            0,
        )
        .await
        .expect("acquire");
        store
            .complete_turn(acquired.owner(), &messages, HistoryProjection::default())
            .await
            .expect("complete");
        drop(acquired);
        session
            .turn("hello")
            .options(options())
            .host_id("before")
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
            .turn("hello")
            .options(options())
            .host_id("before")
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
                    &ModelSwitch::new(
                        ExecutionIdentity::new("b", "1").expect("identity"),
                        None,
                        ModelCapabilities::new(unbounded_model_profile()),
                        0,
                    )
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
