//! Model-aware context projection contract shared by both built-in stores.

use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ag_harness::{
    ContextBudget, ContextEstimator, ExecutionIdentity, Harness, HostTurnStatus, Model,
    ModelCapabilities, ModelCompletion, ModelError, ModelMessage, ModelMetadata, ModelRegistry,
    ModelRequest, ModelResponse, SessionError, SessionStore, ToolCall, ToolDefinition, TurnError,
    TurnInput, WriteStatus,
};
use async_trait::async_trait;
use serde_json::json;

use crate::store_conformance_test::{image_input, options, schema, stores};

/// Deterministic flat weights so budget arithmetic stays readable in tests.
struct FlatEstimator;

impl ContextEstimator for FlatEstimator {
    fn message_weight(&self, _message: &ModelMessage) -> u64 {
        10
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        1
    }
}

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

fn budget(max_request_weight: u64, reserved_output_weight: u64) -> ContextBudget {
    ContextBudget::new(NonZeroU64::new(max_request_weight).expect("nonzero budget"))
        .with_reserved_output(reserved_output_weight)
        .expect("reserve within budget")
}

fn registry(requests: &Arc<Mutex<Vec<ModelRequest>>>) -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    let registrations: [(&'static str, Option<ContextBudget>); 6] = [
        ("small", Some(budget(35, 5))),
        ("mid", Some(budget(41, 0))),
        ("huge", Some(budget(101, 0))),
        ("tiny", Some(budget(5, 0))),
        ("tiny-default", Some(budget(4, 0))),
        ("vision", Some(budget(60, 0))),
    ];
    for (name, context_budget) in registrations {
        registry
            .register(
                ExecutionIdentity::new(name, "1").expect("identity"),
                RecordingModel {
                    name,
                    requests: Arc::clone(requests),
                },
                ModelCapabilities {
                    context_budget,
                    image_input: name == "vision",
                    native_continuation: true,
                    tool_calls: true,
                },
            )
            .expect("register");
    }

    registry
}

fn user_texts(request: &ModelRequest) -> Vec<&str> {
    request
        .messages()
        .iter()
        .filter_map(|message| match message {
            ModelMessage::User(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn projection_bounds_requests_and_replays_without_continuation() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let harness = Harness::from_registry(&registry, "small")
            .expect("harness")
            .context_estimator(FlatEstimator)
            .store(Arc::clone(&store));
        let mut session = harness
            .session("projected", schema())
            .create()
            .await
            .expect("session");
        let mut fresh = harness.resume("projected").await.expect("second handle");

        // Act
        let first = session
            .submit("first", "one", options())
            .await
            .expect("first turn");
        let second = fresh
            .submit("second", "two", options())
            .await
            .expect("turn on a handle that never saw the first turn");
        let third = session
            .submit("third", "three", options())
            .await
            .expect("third turn");

        // Assert
        let loaded = store.load_session("projected").await.expect("canonical");
        assert_eq!(loaded.turns.len(), 3);
        assert!(matches!(
            &loaded.turns[0][0],
            ModelMessage::User(text) if text == "one"
        ));
        let recovered = session
            .recover("first")
            .await
            .expect("recover")
            .expect("record");
        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 3);
        // A budget of 35 minus reserved 5 and the 10-weight input message
        // keeps one 20-weight turn.
        assert_eq!(user_texts(&requests[0]), ["one"]);
        assert_eq!(user_texts(&requests[1]), ["one", "two"]);
        assert_eq!(user_texts(&requests[2]), ["two", "three"]);
        // Each report states what projection replayed and evicted.
        assert_eq!(first.report().history().replayed_turns(), 0);
        assert_eq!(first.report().history().evicted_turns(), 0);
        assert_eq!(second.report().history().replayed_turns(), 1);
        assert_eq!(second.report().history().evicted_turns(), 0);
        assert_eq!(third.report().history().replayed_turns(), 1);
        assert_eq!(third.report().history().evicted_turns(), 1);
        assert!(!third.report().history().checkpoint_replayed());
        // Loading is silently bounded by the byte replay budget, so a budgeted
        // registration never trusts a provider-side continuation.
        assert!(
            requests
                .iter()
                .all(|request| request.provider_session_id().is_none())
        );
        assert_eq!(recovered.model.expect("provenance").generation, 0);
        let HostTurnStatus::Completed(outcome) = recovered.status else {
            unreachable!("first turn completed");
        };
        assert_eq!(outcome.output(), first.output());
    }
}

#[tokio::test]
async fn oversized_mandatory_content_fails_before_acquisition() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let harness = Harness::from_registry(&registry, "tiny")
            .expect("harness")
            .context_estimator(FlatEstimator)
            .store(Arc::clone(&store));
        let mut session = harness
            .session("oversized", schema())
            .system_prompt("sys")
            .create()
            .await
            .expect("session");

        // Act
        let rejected = session.submit("never", "hi", options()).await;

        // Assert
        assert!(matches!(
            rejected,
            Err(SessionError::Turn(TurnError::ContextBudgetExceeded {
                budget: 5,
                required: 20,
            }))
        ));
        assert!(
            session.recover("never").await.expect("recover").is_none(),
            "a rejected request must not acquire a turn"
        );
        let loaded = store.load_session("oversized").await.expect("load");
        assert_eq!(loaded.turns.len(), 0);
        assert_eq!(requests.lock().expect("requests").len(), 0);
    }
}

#[tokio::test]
async fn one_shot_turns_admit_input_with_the_default_estimator() {
    // Arrange
    let requests = Arc::new(Mutex::new(Vec::new()));
    let registry = registry(&requests);
    let harness = Harness::from_registry(&registry, "tiny-default").expect("harness");

    // Act
    let rejected = harness.run_once("0123456789abcdef", schema()).await;

    // Assert
    // Sixteen bytes weigh ceil(16 / 4) + 4 = 8 against a budget of 4.
    assert!(matches!(
        rejected,
        Err(TurnError::ContextBudgetExceeded {
            budget: 4,
            required: 8,
        })
    ));
    assert_eq!(requests.lock().expect("requests").len(), 0);
}

#[tokio::test]
async fn switching_models_applies_the_target_budget_to_whole_tool_groups() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let harness = Harness::from_registry(&registry, "mid")
            .expect("harness")
            .context_estimator(FlatEstimator)
            .store(Arc::clone(&store));
        let mut session = harness
            .session("groups", schema())
            .create()
            .await
            .expect("session");
        seed_tool_and_text_turns(&store).await;

        // Act
        session.send("q1").await.expect("trimmed turn");
        session
            .switch_model(&registry, "huge")
            .await
            .expect("switch");
        session.send("q2").await.expect("roomy turn");

        // Assert
        let writes = session.writes().await.expect("journal");
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].status, WriteStatus::Applied);
        let loaded = store.load_session("groups").await.expect("canonical");
        assert_eq!(loaded.turns.len(), 4);
        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 2);
        // Budget 41 keeps only the 20-weight text turn: the four-message tool
        // turn is dropped wholesale, never split.
        assert_eq!(user_texts(&requests[0]), ["seed text", "q1"]);
        assert!(!requests[0].messages().iter().any(|message| matches!(
            message,
            ModelMessage::AssistantToolCalls(_) | ModelMessage::ToolResult { .. }
        )));
        assert_eq!(requests[0].provider_session_id(), None);
        // Budget 101 fits all three turns, so the tool group returns intact.
        assert_eq!(
            user_texts(&requests[1]),
            ["seed tool", "seed text", "q1", "q2"]
        );
        assert!(requests[1].messages().iter().any(|message| matches!(
            message,
            ModelMessage::AssistantToolCalls(calls) if calls.len() == 1
        )));
        assert!(requests[1].messages().iter().any(|message| matches!(
            message,
            ModelMessage::ToolResult { call_id, .. } if call_id == "call-1"
        )));
    }
}

/// Seeds one completed four-message tool-group turn with an applied write and
/// one completed two-message text turn directly through the store contract.
async fn seed_tool_and_text_turns(store: &Arc<dyn SessionStore>) {
    let call = ToolCall::from_json("call-1".to_string(), "read", r#"{"path":"name.txt"}"#, None)
        .expect("tool call");
    let tool_turn = store
        .begin_turn(
            Arc::clone(store),
            "groups",
            &TurnInput::from("seed tool"),
            &options(),
            0,
        )
        .await
        .expect("seed tool turn");
    let write = store
        .write_intent(
            tool_turn.owner(),
            "call-1",
            Path::new("/repo"),
            "name.txt",
            None,
            b"content",
        )
        .await
        .expect("write intent");
    store
        .finish_write(tool_turn.owner(), write, true)
        .await
        .expect("write outcome");
    store
        .complete_turn(
            tool_turn.owner(),
            &[
                ModelMessage::AssistantToolCalls(vec![call]),
                ModelMessage::ToolResult {
                    call_id: "call-1".to_string(),
                    content: "name".to_string(),
                    name: "read".to_string(),
                },
                ModelMessage::Assistant(r#"{"answer":"seeded"}"#.to_string()),
            ],
            Some("seeded-continuation"),
        )
        .await
        .expect("complete tool turn");
    let text_turn = store
        .begin_turn(
            Arc::clone(store),
            "groups",
            &TurnInput::from("seed text"),
            &options(),
            0,
        )
        .await
        .expect("seed text turn");
    store
        .complete_turn(
            text_turn.owner(),
            &[ModelMessage::Assistant(
                r#"{"answer":"seeded"}"#.to_string(),
            )],
            Some("seeded-continuation"),
        )
        .await
        .expect("complete text turn");
}

#[tokio::test]
async fn image_history_projects_at_its_encoded_weight() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registry = registry(&requests);
        let harness = Harness::from_registry(&registry, "vision")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("imaged", schema())
            .create()
            .await
            .expect("session");

        // Act
        session
            .send(image_input("look", &[0_u8; 100], "now"))
            .await
            .expect("image turn");
        let next = session.send("next").await.expect("text turn");
        session.send("again").await.expect("later turn");
        let oversized = session.send(image_input("look", &[1_u8; 200], "now")).await;

        // Assert
        let loaded = store.load_session("imaged").await.expect("canonical");
        assert!(matches!(
            &loaded.turns[0][0],
            ModelMessage::UserInput(input) if input.has_images()
        ));
        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 3);
        // The 57-weight image turn exceeds the 55 weights left after the
        // current input, so replay drops it; budgeted registrations never
        // reuse native continuation.
        assert!(
            !requests[1]
                .messages()
                .iter()
                .any(|message| matches!(message, ModelMessage::UserInput(_)))
        );
        assert_eq!(user_texts(&requests[1]), ["next"]);
        assert_eq!(requests[1].provider_session_id(), None);
        assert_eq!(next.report().history().evicted_turns(), 1);
        assert_eq!(next.report().history().replayed_turns(), 0);
        assert_eq!(user_texts(&requests[2]), ["next", "again"]);
        assert!(matches!(
            oversized,
            Err(SessionError::Turn(TurnError::ContextBudgetExceeded {
                budget: 60,
                ..
            }))
        ));
    }
}
