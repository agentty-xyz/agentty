//! Compaction checkpoint contract shared by both built-in stores.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ag_harness::catalog::{Capabilities, CatalogError, ModelSpec};
use ag_harness::event::TurnIndex;
use ag_harness::lifecycle::{LifecycleEvent, LifecycleEventKind, LifecycleObserver};
use ag_harness::model::{
    ModelCapabilities, ModelCompletion, ModelMessage, ModelMetadata, ModelProfile, ModelRegistry,
    ModelRequest, ModelResponse,
};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::{CheckpointError, SessionCheckpoint};
use ag_harness::{
    ErrorKind, Harness, HarnessError, Model, ModelCatalog, ModelError, ModelSelectionError,
    ModelSelector, OutputSchema, Role, SessionError, TurnError, TurnRange, TurnRecord,
};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Notify;

use crate::model_profile_fixture::unbounded_model_profile;
use crate::public_store::{schema, stores};

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
    provider: &'static str,
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
        Self::hosted(name, name, requests)
    }

    /// Returns a model reporting `provider` and `name` as its metadata.
    fn hosted(
        provider: &'static str,
        name: &'static str,
        requests: &Arc<Mutex<Vec<ModelRequest>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            generations: AtomicUsize::new(0),
            mode: Mutex::new(SummaryMode::Valid),
            name,
            pause: Mutex::new(false),
            provider,
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
        Some(ModelMetadata::new(self.provider, self.name).expect("metadata"))
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
            ModelCapabilities::new(profile).with_tool_calls(true),
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
        assert_eq!(
            reopened.replayed_range(),
            TurnRange::Span(TurnIndex::new(2)..TurnIndex::new(3)),
            "the replayed range starts after the checkpoint"
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
        assert!(
            replayed_turns(&harness, "repeat").await.is_empty(),
            "every completed turn is covered"
        );
        assert_eq!(
            stored_checkpoint(&harness, "repeat")
                .await
                .expect("checkpoint")
                .covered_through(),
            1
        );
    }
}

/// Returns the checkpoint a handle reopened on `id` reads.
async fn stored_checkpoint(harness: &Harness, id: &str) -> Option<SessionCheckpoint> {
    harness
        .resume(id)
        .await
        .expect("resume")
        .checkpoint()
        .cloned()
}

/// Returns the settled turns the next turn of a handle reopened on `id`
/// replays.
async fn replayed_turns(harness: &Harness, id: &str) -> Vec<TurnRecord> {
    let session = harness.resume(id).await.expect("resume");

    session
        .transcript(session.replayed_range())
        .await
        .expect("replayed turns")
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
        assert_eq!(
            stored_checkpoint(&harness, "resilient")
                .await
                .expect("prior checkpoint")
                .summary(),
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
            stored_checkpoint(&harness, "cancel").await.is_none(),
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
        assert_eq!(replayed_turns(&target_harness, "switch").await.len(), 1);
    }
}

fn capabilities() -> ModelCapabilities {
    ModelCapabilities::new(unbounded_model_profile()).with_tool_calls(true)
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
        assert!(
            replayed_turns(&harness, "bounded").await.is_empty(),
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
        let replayed = replayed_turns(&harness, "history").await;
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
        assert!(
            stored_checkpoint(&harness, "inflated").await.is_none(),
            "nothing is published"
        );
        assert_eq!(
            replayed_turns(&harness, "inflated").await.len(),
            1,
            "the turn stays uncovered"
        );
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
            stored_checkpoint(&harness, "fenced").await.is_none(),
            "a fenced handle publishes nothing"
        );
    }
}

/// Cataloged compacting models, each recording the requests it answered.
struct CatalogedModels {
    catalog: Arc<ModelCatalog>,
    cheap: Arc<Mutex<Vec<ModelRequest>>>,
    review: Arc<Mutex<Vec<ModelRequest>>>,
    smart: Arc<Mutex<Vec<ModelRequest>>>,
}

/// Provider every cataloged compacting model reports, so the default
/// summary model shares the session model's provider.
const HOST_PROVIDER: &str = "host";

/// Returns a catalog of `smart`, `cheap`, and `review` compacting models
/// with structured output, assigning `smart` the smart role, `review` a
/// host-defined review role, and `cheap` the cheap role when
/// `with_cheap_role` is set.
fn cataloged_models(with_cheap_role: bool) -> CatalogedModels {
    cataloged_models_with(
        with_cheap_role,
        unbounded_model_profile(),
        |model: Arc<CompactingModel>| model,
    )
}

/// Returns [`cataloged_models`] with `cheap_profile` for `cheap`, which
/// `wrap_cheap` may wrap in another model.
fn cataloged_models_with<M: Model + 'static>(
    with_cheap_role: bool,
    cheap_profile: ModelProfile,
    wrap_cheap: impl FnOnce(Arc<CompactingModel>) -> Arc<M>,
) -> CatalogedModels {
    let (smart, cheap, review) = (
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(Vec::new())),
    );
    let structured = Capabilities::NONE.with_structured_output(true);
    let spec = |id: &str, profile: ModelProfile| {
        ModelSpec::new(id)
            .with_profile(profile)
            .with_capabilities(structured)
    };
    let builder = ModelCatalog::builder()
        .model(
            spec("host/smart", unbounded_model_profile()),
            CompactingModel::hosted(HOST_PROVIDER, "smart", &smart),
        )
        .model(
            spec("host/cheap", cheap_profile),
            wrap_cheap(CompactingModel::hosted(HOST_PROVIDER, "cheap", &cheap)),
        )
        .model(
            spec("host/review", unbounded_model_profile()),
            CompactingModel::hosted(HOST_PROVIDER, "review", &review),
        )
        .assign(Role::SMART, ["host/smart"])
        .assign(Role::new("review"), ["host/review"]);
    let builder = if with_cheap_role {
        builder.assign(Role::CHEAP, ["host/cheap"])
    } else {
        builder
    };

    CatalogedModels {
        catalog: Arc::new(builder.build().expect("catalog")),
        cheap,
        review,
        smart,
    }
}

/// Counts the checkpoint generation requests among `requests`.
fn generations(requests: &Mutex<Vec<ModelRequest>>) -> usize {
    requests
        .lock()
        .expect("requests")
        .iter()
        .filter(|request| CompactingModel::is_generation(request))
        .count()
}

#[tokio::test]
async fn compaction_runs_on_the_cheap_role_and_attributes_generation_to_it() {
    for store in stores().await {
        // Arrange
        let models = cataloged_models(true);
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&events);
        let harness = Harness::builder()
            .catalog(Arc::clone(&models.catalog))
            .store(Arc::clone(&store))
            .lifecycle_observer(move |event: LifecycleEvent| {
                recorded.lock().expect("events").push(event);
            })
            .build()
            .expect("harness");
        let mut session = harness
            .session("cheap", schema())
            .create()
            .await
            .expect("session");
        session.send("first").await.expect("first");
        events.lock().expect("events").clear();

        // Act
        let checkpoint = session
            .compact()
            .await
            .expect("compact")
            .expect("checkpoint");

        // Assert
        assert_eq!(checkpoint.provider(), Some(HOST_PROVIDER));
        assert_eq!(checkpoint.model(), Some("cheap"));
        let stored = stored_checkpoint(&harness, "cheap")
            .await
            .expect("stored checkpoint");
        assert_eq!(
            (stored.provider(), stored.model()),
            (Some(HOST_PROVIDER), Some("cheap"))
        );
        assert_eq!(generations(&models.cheap), 1);
        assert_eq!(generations(&models.smart), 0);
        let started: Vec<_> = events
            .lock()
            .expect("events")
            .iter()
            .filter_map(|event| match event.kind() {
                LifecycleEventKind::ModelRequestStarted { model, .. } => {
                    Some(model.as_ref().map(|model| model.model().to_string()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(started, vec![Some("cheap".to_string())]);
    }
}

#[tokio::test]
async fn compaction_falls_back_to_the_session_model_without_a_cheap_role() {
    for store in stores().await {
        // Arrange
        let models = cataloged_models(false);
        let harness = Harness::builder()
            .catalog(Arc::clone(&models.catalog))
            .store(Arc::clone(&store))
            .build()
            .expect("harness");
        let mut session = harness
            .session("fallback", schema())
            .create()
            .await
            .expect("session");
        session.send("first").await.expect("first");

        // Act
        let checkpoint = session
            .compact()
            .await
            .expect("compact")
            .expect("checkpoint");

        // Assert
        assert_eq!(checkpoint.model(), Some("smart"));
        assert_eq!(generations(&models.smart), 1);
        assert_eq!(generations(&models.cheap), 0);
    }
}

#[tokio::test]
async fn explicit_compaction_models_override_the_summary_model() {
    for store in stores().await {
        // Arrange
        let models = cataloged_models(true);
        let harness = Harness::builder()
            .catalog(Arc::clone(&models.catalog))
            .summary_model_selector(Role::new("review"))
            .store(Arc::clone(&store))
            .build()
            .expect("harness");
        let mut session = harness
            .session("explicit", schema())
            .create()
            .await
            .expect("session");

        // Act
        session.send("one").await.expect("one");
        let configured = session
            .compact()
            .await
            .expect("configured compact")
            .expect("checkpoint");
        session.send("two").await.expect("two");
        let cheap = session
            .compact_with(Role::CHEAP)
            .await
            .expect("cheap compact")
            .expect("checkpoint");
        session.send("three").await.expect("three");
        let smart = session
            .compact_with(ModelSelector::Id("host/smart".into()))
            .await
            .expect("smart compact")
            .expect("checkpoint");
        let unknown = session
            .compact_with(ModelSelector::Id("host/missing".into()))
            .await;

        // Assert
        assert_eq!(configured.model(), Some("review"));
        assert_eq!(cheap.model(), Some("cheap"));
        assert_eq!(smart.model(), Some("smart"));
        assert_eq!(smart.covered_through(), 2);
        assert_eq!(generations(&models.review), 1);
        assert_eq!(generations(&models.cheap), 1);
        assert_eq!(generations(&models.smart), 1);
        assert!(matches!(
            unknown,
            Err(SessionError::ModelSelection(ModelSelectionError::Catalog(
                CatalogError::UnknownModel { .. }
            )))
        ));
        assert_eq!(
            unknown
                .err()
                .and_then(|error| std::error::Error::source(&error).map(ToString::to_string)),
            Some("cannot select the model from the catalog".to_string())
        );
    }
}

#[tokio::test]
async fn compaction_with_a_selector_requires_a_cataloged_session_model() {
    for store in stores().await {
        // Arrange
        let models = cataloged_models(true);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let harness = Harness::builder()
            .model(CompactingModel::shared("direct", &requests))
            .context_budget(unbounded_model_profile().context_budget().expect("budget"))
            .catalog(Arc::clone(&models.catalog))
            .store(Arc::clone(&store))
            .build()
            .expect("harness");
        let mut session = harness
            .session("direct", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");

        // Act
        let selected = session.compact_with(Role::CHEAP).await;
        let checkpoint = session
            .compact()
            .await
            .expect("compact")
            .expect("checkpoint");

        // Assert
        let error = selected.expect_err("an explicit model replaces the catalog");
        assert!(matches!(
            error,
            SessionError::ModelSelection(ModelSelectionError::NoCatalog)
        ));
        assert_eq!(
            std::error::Error::source(&error).map(ToString::to_string),
            Some("the session's model did not come from a catalog".to_string())
        );
        assert_eq!(HarnessError::from(error).kind(), ErrorKind::Config);
        assert_eq!(checkpoint.model(), Some("direct"));
        assert_eq!(generations(&models.cheap), 0);
    }
}

#[tokio::test]
async fn compaction_sizes_its_source_with_the_summary_models_profile() {
    for store in stores().await {
        // Arrange: the cheap model's small profile admits only some of the
        // long turns that the unbounded session model holds.
        let models = cataloged_models_with(true, ModelProfile::new(650), |model| model);
        let harness = Harness::builder()
            .catalog(Arc::clone(&models.catalog))
            .store(Arc::clone(&store))
            .build()
            .expect("harness");
        let mut session = harness
            .session("sized", schema())
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
        let cheap = session
            .compact()
            .await
            .expect("cheap compact")
            .expect("checkpoint");
        let smart = session
            .compact_with(Role::SMART)
            .await
            .expect("smart compact")
            .expect("checkpoint");

        // Assert
        assert!(cheap.covered_through() < 3, "the cheap source stops early");
        assert_eq!(cheap.model(), Some("cheap"));
        assert_eq!(smart.model(), Some("smart"));
        assert_eq!(
            smart.covered_through(),
            3,
            "the session model covers the rest"
        );
        assert_eq!(generations(&models.smart), 1);
    }
}

/// Model that forwards to a compacting model but reports no metadata and
/// can reject every output schema.
struct OpaqueModel {
    inner: Arc<CompactingModel>,
    rejects_schemas: bool,
}

#[async_trait]
impl Model for OpaqueModel {
    fn validate_schema(&self, _schema: &OutputSchema) -> Result<(), ModelError> {
        if self.rejects_schemas {
            return Err(ModelError::UnsupportedOutputSchema {
                reason: "no structured output".to_string(),
            });
        }

        Ok(())
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.inner.complete(request).await
    }
}

#[tokio::test]
async fn summary_models_without_metadata_leave_provenance_unknown() {
    for store in stores().await {
        // Arrange
        let models = cataloged_models_with(true, unbounded_model_profile(), |inner| {
            Arc::new(OpaqueModel {
                inner,
                rejects_schemas: false,
            })
        });
        // A model without metadata shares no known provider, so only an
        // explicit selector summarizes on it.
        let harness = Harness::builder()
            .catalog(Arc::clone(&models.catalog))
            .summary_model_selector(Role::CHEAP)
            .store(Arc::clone(&store))
            .build()
            .expect("harness");
        let mut session = harness
            .session("opaque", schema())
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

        // Assert
        let stored = stored_checkpoint(&harness, "opaque")
            .await
            .expect("stored checkpoint");
        assert_eq!((checkpoint.provider(), checkpoint.model()), (None, None));
        assert_eq!((stored.provider(), stored.model()), (None, None));
        assert_eq!(generations(&models.cheap), 1);
    }
}

#[tokio::test]
async fn summary_models_that_reject_the_checkpoint_schema_fail_before_generation() {
    for store in stores().await {
        // Arrange
        let models = cataloged_models_with(true, unbounded_model_profile(), |inner| {
            Arc::new(OpaqueModel {
                inner,
                rejects_schemas: true,
            })
        });
        let harness = Harness::builder()
            .catalog(Arc::clone(&models.catalog))
            .store(Arc::clone(&store))
            .build()
            .expect("harness");
        let mut session = harness
            .session("rejecting", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");

        // Act
        let result = session.compact_with(Role::CHEAP).await;

        // Assert
        assert!(matches!(
            &result,
            Err(SessionError::ModelSelection(ModelSelectionError::Unsupported { id }))
                if id.as_str() == "host/cheap"
        ));
        assert_eq!(
            result
                .err()
                .and_then(|error| std::error::Error::source(&error).map(ToString::to_string)),
            Some("model `host/cheap` cannot write summaries".to_string())
        );
        assert_eq!(generations(&models.cheap), 0);
    }
}

#[tokio::test]
async fn switching_to_a_registered_model_moves_compaction_to_it() {
    for store in stores().await {
        // Arrange
        let models = cataloged_models(true);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let target = CompactingModel::shared("target", &requests);
        let registry = registry(Arc::clone(&target), unbounded_model_profile());
        let harness = Harness::builder()
            .catalog(Arc::clone(&models.catalog))
            .store(Arc::clone(&store))
            .build()
            .expect("harness");
        let mut session = harness
            .session("switched", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");

        // Act
        session
            .switch_model(&registry, "target")
            .await
            .expect("switch");
        session.send("two").await.expect("two");
        let checkpoint = session
            .compact()
            .await
            .expect("compact")
            .expect("checkpoint");
        let selected = session.compact_with(Role::CHEAP).await;

        // Assert
        assert_eq!(checkpoint.model(), Some("target"));
        assert_eq!(target.generations.load(Ordering::SeqCst), 1);
        assert_eq!(generations(&models.cheap), 0);
        assert!(matches!(
            selected,
            Err(SessionError::ModelSelection(ModelSelectionError::NoCatalog))
        ));
    }
}
