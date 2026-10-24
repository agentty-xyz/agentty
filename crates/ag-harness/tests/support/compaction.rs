//! Compaction checkpoint contract shared by both built-in stores.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ag_harness::lifecycle::{LifecycleEvent, LifecycleEventKind, LifecycleObserver};
use ag_harness::model::{
    ModelCapabilities, ModelCompletion, ModelMessage, ModelMetadata, ModelProfile, ModelRegistry,
    ModelRequest, ModelResponse,
};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::{CheckpointError, SessionCheckpoint, SessionStore};
use ag_harness::{Harness, Model, ModelError, SessionError, TurnError};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::model_profile_fixture::unbounded_model_profile;
use crate::store_fixture::{schema, stores};

/// Model that answers ordinary turns and, when it sees the compaction system
/// instruction, returns a configurable structured summary. Generation can be
/// paused, made to fail, or made to emit summaries that violate the checkpoint
/// schema.
struct CompactingModel {
    entered: Notify,
    generations: AtomicUsize,
    mode: Mutex<SummaryMode>,
    name: &'static str,
    pause: Mutex<bool>,
    released: Notify,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[derive(Clone)]
enum SummaryMode {
    Valid,
    Fail,
    Inflated,
    Malformed,
    Oversized,
}

impl CompactingModel {
    fn shared(name: &'static str, requests: &Arc<Mutex<Vec<ModelRequest>>>) -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            generations: AtomicUsize::new(0),
            mode: Mutex::new(SummaryMode::Valid),
            name,
            pause: Mutex::new(false),
            released: Notify::new(),
            requests: Arc::clone(requests),
        })
    }

    fn set_mode(&self, mode: SummaryMode) {
        *self.mode.lock().expect("mode") = mode;
    }

    fn set_pause(&self, pause: bool) {
        *self.pause.lock().expect("pause") = pause;
    }

    fn is_generation(request: &ModelRequest) -> bool {
        request.messages().iter().any(|message| {
            matches!(message, ModelMessage::System(text) if text.starts_with("Summarize the"))
        })
    }
}

#[async_trait]
impl Model for CompactingModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(ModelMetadata::new(self.name, self.name).expect("metadata"))
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.requests
            .lock()
            .expect("requests")
            .push(request.clone());
        if !Self::is_generation(&request) {
            return Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({"answer": self.name}),
            )));
        }
        self.generations.fetch_add(1, Ordering::SeqCst);
        if *self.pause.lock().expect("pause") {
            self.entered.notify_one();
            self.released.notified().await;
        }
        let mode = self.mode.lock().expect("mode").clone();
        match mode {
            SummaryMode::Valid => Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({
                    "context": format!("summary from {}", self.name),
                    "decisions": ["carry forward"],
                    "state": "compacted"
                }),
            ))),
            SummaryMode::Fail => Err(ModelError::request(std::io::Error::other(
                "generation failed",
            ))),
            SummaryMode::Inflated => Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({
                    "context": "c".repeat(3000),
                    "decisions": [],
                    "state": "compacted"
                }),
            ))),
            SummaryMode::Malformed => Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({"context": "missing required fields"}),
            ))),
            SummaryMode::Oversized => {
                let decisions: Vec<String> = (0..32).map(|_| "d".repeat(400)).collect();

                Ok(ModelCompletion::from_response(ModelResponse::Output(
                    json!({
                        "context": "c".repeat(4000),
                        "decisions": decisions,
                        "state": "s".repeat(2000)
                    }),
                )))
            }
        }
    }
}

fn registry(model: Arc<CompactingModel>, profile: ModelProfile) -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    let name = model.name;
    registry
        .register_shared(
            ExecutionIdentity::new(name, "1").expect("identity"),
            model,
            ModelCapabilities {
                image_input: false,
                profile,
                tool_calls: true,
            },
        )
        .expect("register");

    registry
}

fn user_texts(request: &ModelRequest) -> Vec<String> {
    request
        .messages()
        .iter()
        .filter_map(|message| match message {
            ModelMessage::User(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn summary_message(request: &ModelRequest) -> Option<String> {
    request.messages().iter().find_map(|message| match message {
        ModelMessage::User(text) if text.contains("Compaction checkpoint summarizing") => {
            Some(text.clone())
        }
        _ => None,
    })
}

#[tokio::test]
async fn checkpoint_projects_summary_and_survives_reopen() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("keeper", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "keeper")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("reopen", schema())
            .create()
            .await
            .expect("session");
        session.send("first").await.expect("first");
        session.send("second").await.expect("second");

        // Act
        let published = session
            .compact()
            .await
            .expect("compact")
            .expect("checkpoint");
        let third_outcome = session.send("third").await.expect("post-compaction turn");
        let reopened = harness.resume("reopen").await.expect("resume");

        // Assert
        assert_eq!(published.covered_through(), 1);
        assert!(third_outcome.report().history().checkpoint_replayed());
        assert_eq!(third_outcome.report().history().replayed_turns(), 0);
        assert_eq!(published.model(), Some("keeper"));
        assert_eq!(
            reopened.checkpoint().expect("checkpoint").summary(),
            published.summary()
        );
        let loaded = store.load_session("reopen").await.expect("canonical");
        assert_eq!(
            loaded.turns.len(),
            1,
            "checkpoint bounds replay to uncovered turns"
        );
        assert!(
            matches!(&loaded.turns[0].1.messages[0], ModelMessage::User(text) if text == "third")
        );
        let requests = requests.lock().expect("requests");
        let third = requests.last().expect("third request");
        assert!(
            summary_message(third).is_some(),
            "summary replaces covered turns"
        );
        assert!(!user_texts(third).iter().any(|text| text == "first"));
    }
}

#[tokio::test]
async fn repeated_compaction_extends_coverage_without_gaps() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("repeat", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "repeat")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("repeat", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");

        // Act
        let first = session
            .compact()
            .await
            .expect("first compact")
            .expect("checkpoint");
        session.send("two").await.expect("two");
        let second = session
            .compact()
            .await
            .expect("second compact")
            .expect("checkpoint");
        let noop = session.compact().await.expect("third compact");

        // Assert
        assert_eq!(first.covered_through(), 0);
        assert_eq!(second.covered_through(), 1);
        assert!(noop.is_none(), "no uncovered completed turn remains");
        assert_eq!(model.generations.load(Ordering::SeqCst), 2);
        let loaded = store.load_session("repeat").await.expect("canonical");
        assert!(loaded.turns.is_empty(), "every completed turn is covered");
        assert_eq!(loaded.checkpoint.expect("checkpoint").covered_through(), 1);
    }
}

#[tokio::test]
async fn stale_publication_is_rejected_after_a_concurrent_turn() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("stale", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "stale")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("stale", schema())
            .create()
            .await
            .expect("session");
        session.send("only").await.expect("only");
        let checkpoint =
            SessionCheckpoint::new(0, 0, Some("stale".into()), Some("stale".into()), summary())
                .expect("checkpoint");

        // Act: publish a checkpoint whose generation no longer matches after a
        // switch, and one whose boundary exceeds the completed turns.
        session
            .switch_model(&registry, "stale")
            .await
            .expect("switch back is a no-op key");
        let stale_generation = store.publish_checkpoint("stale", &checkpoint).await;
        let beyond_completed = SessionCheckpoint::new(
            5,
            session_generation(&store, "stale").await,
            Some("stale".into()),
            Some("stale".into()),
            summary(),
        )
        .expect("checkpoint");
        let beyond = store.publish_checkpoint("stale", &beyond_completed).await;

        // Assert
        assert!(matches!(
            stale_generation,
            Err(SessionError::CheckpointStale { .. })
        ));
        assert!(matches!(beyond, Err(SessionError::CheckpointStale { .. })));
        assert!(
            store
                .load_session("stale")
                .await
                .expect("load")
                .checkpoint
                .is_none(),
            "a rejected publication changes nothing"
        );
    }
}

async fn session_generation(store: &Arc<dyn SessionStore>, id: &str) -> i64 {
    store.load_session(id).await.expect("load").model_generation
}

fn summary() -> Value {
    json!({"context": "c", "decisions": [], "state": "s"})
}

#[tokio::test]
async fn generation_failure_preserves_a_usable_prior_checkpoint() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("resilient", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "resilient")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("resilient", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");
        let first = session
            .compact()
            .await
            .expect("first compact")
            .expect("checkpoint");
        session.send("two").await.expect("two");

        // Act
        model.set_mode(SummaryMode::Fail);
        let failed = session.compact().await;
        model.set_mode(SummaryMode::Malformed);
        let malformed = session.compact().await;
        model.set_mode(SummaryMode::Oversized);
        let oversized = session.compact().await;

        // Assert
        assert!(matches!(
            failed,
            Err(SessionError::Turn(TurnError::Model(_)))
        ));
        assert!(matches!(
            malformed,
            Err(SessionError::Turn(TurnError::Model(_)))
        ));
        assert!(matches!(
            oversized,
            Err(SessionError::Checkpoint(
                CheckpointError::SummaryTooLarge { .. }
            ))
        ));
        let loaded = store.load_session("resilient").await.expect("load");
        assert_eq!(
            loaded.checkpoint.expect("prior checkpoint").summary(),
            first.summary(),
            "failed generation leaves the earlier checkpoint intact"
        );
        // Projection still works: turn two remains uncovered and replayable.
        session.send("three").await.expect("post-failure turn");
        assert!(
            summary_message(requests.lock().expect("requests").last().expect("request")).is_some()
        );
    }
}

#[tokio::test]
async fn cancelled_generation_publishes_nothing() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("cancel", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "cancel")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("cancel", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");
        model.set_pause(true);

        // Act
        let mut compaction = Box::pin(session.compact());
        tokio::select! {
            biased;
            () = model.entered.notified() => {}
            _ = &mut compaction => unreachable!("generation is paused"),
        }
        // Dropping the boxed future cancels generation before it can publish.
        drop(compaction);
        model.released.notify_one();

        // Assert
        assert!(
            store
                .load_session("cancel")
                .await
                .expect("load")
                .checkpoint
                .is_none(),
            "cancelling before publication leaves no checkpoint"
        );
    }
}

#[tokio::test]
async fn model_switch_keeps_checkpoints_and_journals_intact() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let first_model = CompactingModel::shared("origin", &requests);
        let second_model = CompactingModel::shared("target", &requests);
        let mut registry = ModelRegistry::new();
        registry
            .register_shared(
                ExecutionIdentity::new("origin", "1").expect("identity"),
                Arc::clone(&first_model) as Arc<dyn Model>,
                capabilities(),
            )
            .expect("register origin");
        registry
            .register_shared(
                ExecutionIdentity::new("target", "1").expect("identity"),
                Arc::clone(&second_model) as Arc<dyn Model>,
                capabilities(),
            )
            .expect("register target");
        let harness = Harness::from_registry(&registry, "origin")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("switch", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");

        // Act
        let checkpoint = session
            .compact()
            .await
            .expect("compact")
            .expect("checkpoint");
        session
            .switch_model(&registry, "target")
            .await
            .expect("switch");
        session.send("two").await.expect("post-switch turn");

        // Assert
        assert_eq!(checkpoint.model(), Some("origin"));
        let target_harness = Harness::from_registry(&registry, "target")
            .expect("harness")
            .store(Arc::clone(&store));
        let reopened = target_harness.resume("switch").await.expect("resume");
        assert_eq!(
            reopened.checkpoint().expect("checkpoint").summary(),
            checkpoint.summary(),
            "switching models preserves the existing checkpoint and its provenance"
        );
        let loaded = store.load_session("switch").await.expect("load");
        assert_eq!(loaded.turns.len(), 1);
        assert!(
            matches!(&loaded.turns[0].1.messages[0], ModelMessage::User(text) if text == "two")
        );
    }
}

fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        image_input: false,
        profile: unbounded_model_profile(),
        tool_calls: true,
    }
}

#[tokio::test]
async fn bounded_generation_falls_back_to_recent_turns_under_budget() {
    for store in stores().await {
        // Arrange: a budget large enough for generation but too small to weigh
        // every uncovered turn into the source at once.
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("bounded", &requests);
        let registry = registry(Arc::clone(&model), ModelProfile::new(4_000));
        let harness = Harness::from_registry(&registry, "bounded")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("bounded", schema())
            .create()
            .await
            .expect("session");
        for index in 0..4 {
            session.send(format!("turn {index}")).await.expect("turn");
        }

        // Act
        let checkpoint = session
            .compact()
            .await
            .expect("compact")
            .expect("checkpoint");

        // Assert
        assert_eq!(checkpoint.covered_through(), 3);
        assert_eq!(model.generations.load(Ordering::SeqCst), 1);
        let loaded = store.load_session("bounded").await.expect("load");
        assert!(
            loaded.turns.is_empty(),
            "generation covers every completed turn"
        );
        // The projected post-compaction request stays within the model budget.
        session.send("after").await.expect("post-compaction turn");
    }
}

#[tokio::test]
async fn compaction_without_completed_turns_is_a_noop() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("idle", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "idle")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("idle", schema())
            .create()
            .await
            .expect("session");

        // Act
        let compacted = session.compact().await.expect("compact");

        // Assert
        assert!(
            compacted.is_none(),
            "nothing completed leaves nothing to cover"
        );
        assert_eq!(model.generations.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn bounded_generation_covers_only_the_oldest_turns_its_source_renders() {
    for store in stores().await {
        // Arrange: four long turns cannot all render into a source the budget
        // admits, so each generation renders only the oldest uncovered turns
        // that fit and covers exactly those.
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("trimming", &requests);
        let registry = registry(Arc::clone(&model), ModelProfile::new(650));
        let harness = Harness::from_registry(&registry, "trimming")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("trimming", schema())
            .create()
            .await
            .expect("session");
        for index in 0..4 {
            let padding = "x".repeat(700);
            session
                .send(format!("turn {index} {padding}"))
                .await
                .expect("turn");
        }

        // Act
        let mut boundaries = Vec::new();
        while let Some(checkpoint) = session.compact().await.expect("compact") {
            boundaries.push(checkpoint.covered_through());
        }

        // Assert
        let requests = requests.lock().expect("requests");
        let sources: Vec<String> = requests
            .iter()
            .filter(|request| CompactingModel::is_generation(request))
            .map(|request| request.prompt().to_string())
            .collect();
        assert_eq!(sources.len(), boundaries.len());
        assert!(
            sources[0].contains("turn 0") && !sources[0].contains("turn 3"),
            "the first source keeps the oldest turns and drops the newest"
        );
        assert!(
            boundaries.windows(2).all(|pair| pair[0] < pair[1]),
            "every generation advances coverage: {boundaries:?}"
        );
        assert_eq!(
            boundaries.last(),
            Some(&3),
            "coverage reaches the newest turn"
        );
        // A turn is covered only after a source rendered it.
        let mut covered = -1;
        for (source, boundary) in sources.iter().zip(&boundaries) {
            for turn in covered + 1..=*boundary {
                assert!(
                    source.contains(&format!("turn {turn} ")),
                    "turn {turn} is covered through {boundary} without being rendered"
                );
            }
            covered = *boundary;
        }
    }
}

#[tokio::test]
async fn generation_covers_turns_beyond_the_replayed_history_oldest_first() {
    for store in stores().await {
        // Arrange: the history byte limit replays only the newest turns, while
        // the context budget would admit every turn into a source.
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("history", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "history")
            .expect("harness")
            .store(Arc::clone(&store))
            .max_history_bytes(NonZeroUsize::new(700).expect("limit"));
        let mut session = harness
            .session("history", schema())
            .create()
            .await
            .expect("session");
        for index in 0..4 {
            let padding = "y".repeat(250);
            session
                .send(format!("turn {index} {padding}"))
                .await
                .expect("turn");
        }
        let replayed = store.load_session("history").await.expect("load").turns;
        assert!(
            replayed.len() < 4,
            "older turns fall outside replayed history"
        );

        // Act
        let first = session
            .compact()
            .await
            .expect("first compact")
            .expect("checkpoint");
        let mut last = first.covered_through();
        while let Some(checkpoint) = session.compact().await.expect("compact") {
            last = checkpoint.covered_through();
        }

        // Assert
        let requests = requests.lock().expect("requests");
        let first_source = requests
            .iter()
            .find(|request| CompactingModel::is_generation(request))
            .expect("generation")
            .prompt()
            .to_string();
        assert!(
            first_source.contains("turn 0 "),
            "the oldest turn is summarized first"
        );
        for turn in 0..=first.covered_through() {
            assert!(
                first_source.contains(&format!("turn {turn} ")),
                "turn {turn} is covered without being rendered"
            );
        }
        assert!(
            first.covered_through() < 3,
            "turns beyond the source stay uncovered"
        );
        assert_eq!(last, 3, "later generations cover the rest");
    }
}

#[tokio::test]
async fn generation_rejects_a_summary_no_smaller_than_its_source() {
    for store in stores().await {
        // Arrange: a schema-valid summary larger than its one short turn.
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("inflated", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "inflated")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("inflated", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");
        model.set_mode(SummaryMode::Inflated);

        // Act
        let rejected = session.compact().await;

        // Assert
        assert!(matches!(
            rejected,
            Err(SessionError::Checkpoint(
                CheckpointError::SummaryNotSmaller { .. }
            ))
        ));
        let loaded = store.load_session("inflated").await.expect("load");
        assert!(loaded.checkpoint.is_none(), "nothing is published");
        assert_eq!(loaded.turns.len(), 1, "the turn stays uncovered");
    }
}

#[tokio::test]
async fn heavy_summary_is_dropped_from_projection_and_rejected_as_a_source() {
    for store in stores().await {
        // Arrange: a published summary heavier than the whole request budget.
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("heavy", &requests);
        let registry = registry(Arc::clone(&model), ModelProfile::new(500));
        let harness = Harness::from_registry(&registry, "heavy")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("heavy", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");
        let decisions: Vec<String> = (0..8).map(|_| "d".repeat(400)).collect();
        let oversized_summary = json!({
            "context": "c".repeat(4000),
            "decisions": decisions,
            "state": "s".repeat(2000)
        });
        let heavy = SessionCheckpoint::new(0, 0, None, None, oversized_summary).expect("record");
        store
            .publish_checkpoint("heavy", &heavy)
            .await
            .expect("publish");

        // Act: projection admits recent turns without the summary, while
        // generation cannot admit the summary-bearing source at all.
        let two = session.send("two").await.expect("post-checkpoint turn");
        let rejected = session.compact().await;

        // Assert
        assert!(
            summary_message(requests.lock().expect("requests").last().expect("request")).is_none(),
            "a summary beyond the budget falls back to recent history"
        );
        // The checkpoint covers the only completed turn, so nothing is
        // replayed and the dropped summary is visible in the report.
        assert!(!two.report().history().checkpoint_replayed());
        assert_eq!(two.report().history().replayed_turns(), 0);
        assert_eq!(two.report().history().evicted_turns(), 0);
        assert!(matches!(
            rejected,
            Err(SessionError::Turn(TurnError::ContextBudgetExceeded { .. }))
        ));
    }
}

#[tokio::test]
async fn compaction_reports_turn_lifecycle_events() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("observed", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let kinds = Arc::new(Mutex::new(Vec::new()));
        let harness = Harness::from_registry(&registry, "observed")
            .expect("harness")
            .with_lifecycle_observer(KindRecorder {
                kinds: Arc::clone(&kinds),
            })
            .store(Arc::clone(&store));
        let mut session = harness
            .session("observed", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");

        // Act
        session.compact().await.expect("compact");
        session.send("two").await.expect("two");
        model.set_mode(SummaryMode::Fail);
        let failed = session.compact().await;

        // Assert
        assert!(failed.is_err(), "failed generation propagates its error");
        let kinds = kinds.lock().expect("kinds");
        let completed = kinds
            .iter()
            .filter(|kind| matches!(kind, LifecycleEventKind::TurnCompleted { .. }))
            .count();
        let failures = kinds
            .iter()
            .filter(|kind| matches!(kind, LifecycleEventKind::TurnFailed { .. }))
            .count();
        assert_eq!(completed, 3, "both sends and the generation complete");
        assert_eq!(failures, 1, "the failed generation reports one failure");
    }
}

struct KindRecorder {
    kinds: Arc<Mutex<Vec<LifecycleEventKind>>>,
}

impl LifecycleObserver for KindRecorder {
    fn observe(&self, event: LifecycleEvent) {
        self.kinds.lock().expect("kinds").push(event.kind().clone());
    }
}

#[tokio::test]
async fn compaction_requires_a_current_handle() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = CompactingModel::shared("fenced", &requests);
        let registry = registry(Arc::clone(&model), unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "fenced")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut stale = harness
            .session("fenced", schema())
            .create()
            .await
            .expect("session");
        stale.send("one").await.expect("one");
        let mut fresh = harness.resume("fenced").await.expect("resume");
        fresh
            .switch_model(&registry, "fenced")
            .await
            .expect("switch advances generation");

        // Act
        let rejected = stale.compact().await;

        // Assert
        assert!(matches!(rejected, Err(SessionError::StaleModel { .. })));
        assert!(
            store
                .load_session("fenced")
                .await
                .expect("load")
                .checkpoint
                .is_none(),
            "a fenced handle publishes nothing"
        );
    }
}
