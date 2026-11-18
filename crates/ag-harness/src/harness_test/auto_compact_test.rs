//! Automatic compaction unit tests for the trigger threshold, checkpoint
//! publication before admission, the eviction fallback and its backoff,
//! run events, lifecycle attribution, and usage attribution.

use std::collections::VecDeque;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use serde_json::json;
use tokio::sync::Notify;

use super::support::{object_schema, wait_for_fixture};
use crate::budget::{BudgetLedger, Estimate, Reservation};
use crate::catalog::{Capabilities, ModelCatalog, ModelId, ModelSpec, Role};
use crate::compaction::{GENERATION_INSTRUCTIONS, SessionCheckpoint};
use crate::context::{ContextBudget, ContextPolicy};
use crate::error::{ErrorKind, RunError};
use crate::event::{CompactionOutcome, CompactionReport, RunEventKind, RunFailure, TurnIndex};
use crate::fault_store_test::{Fault, FaultStore};
use crate::harness::{EnclosingTurn, Harness, Session};
use crate::lifecycle::{LifecycleEvent, LifecycleEventKind, LifecycleObserver};
use crate::limit::{LimitKind, RunLimits};
use crate::model::{
    CompletionMetadata, CompletionUsage, Model, ModelCompletion, ModelError, ModelMessage,
    ModelMetadata, ModelProfile, ModelRequest, ModelResponse,
};
use crate::reservation::TURN_LEASE_RENEWAL_INTERVAL_SECONDS;
use crate::run::{RunOutcome, RunRequest, Usage};
use crate::session::SessionError;
use crate::store::{MemoryStore, SessionStore};
use crate::turn::{ModelRequestActivity, ModelRequestPurpose, TurnError, TurnOutcome};
use crate::usage::Cost;

/// Weight budget whose uncalibrated share holds a turn's input beside two
/// finished turns of history, but not three.
const BUDGET: u64 = 8_000;

/// Input tokens every compaction response reports.
const COMPACTION_INPUT_TOKENS: u64 = 700;

/// Payload bytes of each turn's input, about a quarter of [`BUDGET`].
const INPUT_BYTES: usize = 8_000;

/// Answers turn requests with a short output and compaction requests with
/// the scripted responses, in order, then with a valid summary.
struct CompactingModel {
    compactions: Mutex<VecDeque<Result<ModelResponse, ModelError>>>,
    /// Compaction requests received so far.
    compactions_seen: AtomicUsize,
    /// Time every request takes before it is answered, if any.
    delay: Option<Duration>,
    /// Turn requests answered before the rest fail, if any.
    failing_turns_after: Option<usize>,
    /// Name reported as model metadata of the `host` provider, if any.
    name: Option<&'static str>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    /// Index of the compaction request that notifies the [`Notify`] when it
    /// starts and then never answers, if any.
    stall: Option<(usize, Arc<Notify>)>,
    /// Turn requests received so far.
    turns_seen: AtomicUsize,
}

impl CompactingModel {
    fn new(
        compactions: Vec<Result<ModelResponse, ModelError>>,
    ) -> (Self, Arc<Mutex<Vec<ModelRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = Self {
            compactions: Mutex::new(compactions.into()),
            compactions_seen: AtomicUsize::new(0),
            delay: None,
            failing_turns_after: None,
            name: None,
            requests: Arc::clone(&requests),
            stall: None,
            turns_seen: AtomicUsize::new(0),
        };

        (model, requests)
    }

    /// Returns this model notifying `started` at compaction request `index`
    /// and never answering it.
    fn stalling_compaction(mut self, index: usize, started: Arc<Notify>) -> Self {
        self.stall = Some((index, started));

        self
    }

    /// Returns this model taking `delay` to answer every request.
    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);

        self
    }

    /// Returns this model failing every turn request after the first
    /// `answered`.
    fn failing_turns_after(mut self, answered: usize) -> Self {
        self.failing_turns_after = Some(answered);

        self
    }

    /// Returns this model reporting `name` as its model metadata, served by
    /// the `host` provider.
    fn named(mut self, name: &'static str) -> Self {
        self.name = Some(name);

        self
    }
}

#[async_trait]
impl Model for CompactingModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        self.name
            .map(|name| ModelMetadata::new("host", name).expect("metadata"))
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let compaction = is_compaction(&request);
        self.requests.lock().expect("requests lock").push(request);
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        if !compaction {
            let answered = self.turns_seen.fetch_add(1, Ordering::SeqCst);
            if self
                .failing_turns_after
                .is_some_and(|limit| answered >= limit)
            {
                return Err(ModelError::InvalidResponse);
            }

            return Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({"summary": "answered"}),
            )));
        }
        let index = self.compactions_seen.fetch_add(1, Ordering::SeqCst);
        if let Some((_, started)) = self.stall.as_ref().filter(|(stall, _)| *stall == index) {
            started.notify_one();
            std::future::pending::<()>().await;
        }
        let scripted = self
            .compactions
            .lock()
            .expect("compactions lock")
            .pop_front();
        let response = scripted.unwrap_or_else(|| Ok(ModelResponse::Output(valid_summary())))?;
        let usage = CompletionUsage::new(
            None,
            None,
            Some(COMPACTION_INPUT_TOKENS),
            Some(20),
            None,
            None,
        );
        let metadata = CompletionMetadata::new("stop".to_string(), None, None, None, Some(usage));

        Ok(ModelCompletion::new(metadata, response))
    }
}

/// Records every lifecycle event.
#[derive(Clone, Default)]
struct LifecycleRecorder {
    events: Arc<Mutex<Vec<LifecycleEvent>>>,
}

impl LifecycleObserver for LifecycleRecorder {
    fn observe(&self, event: LifecycleEvent) {
        self.events.lock().expect("events lock").push(event);
    }
}

fn valid_summary() -> serde_json::Value {
    json!({
        "context": "earlier turns asked about marker cobalt-41",
        "decisions": [],
        "state": "answered"
    })
}

/// Returns a summary that satisfies the checkpoint schema but serializes
/// to more than the bytes a checkpoint may take.
fn oversized_summary() -> serde_json::Value {
    json!({
        "context": "c".repeat(4_000),
        "decisions": vec!["d".repeat(400); 32],
        "state": "answered"
    })
}

/// Returns whether `request` asks for a compaction checkpoint.
fn is_compaction(request: &ModelRequest) -> bool {
    request.messages().first() == Some(&ModelMessage::System(GENERATION_INSTRUCTIONS.to_string()))
}

/// Returns the input of turn `number`, about a quarter of [`BUDGET`].
fn input(number: usize) -> String {
    sized_input(number, INPUT_BYTES)
}

/// Returns the input of turn `number` with `bytes` payload bytes.
fn sized_input(number: usize, bytes: usize) -> String {
    format!("turn-{number} {}", "x".repeat(bytes))
}

fn at(threshold_percent: u8) -> ContextPolicy {
    ContextPolicy::auto_compact_at(threshold_percent).expect("threshold in range")
}

fn harness(model: CompactingModel, policy: ContextPolicy) -> Harness {
    Harness::new(
        model,
        ContextBudget::new(NonZeroU64::new(BUDGET).expect("nonzero budget")),
    )
    .store(Arc::new(MemoryStore::new()))
    .context_policy(policy)
}

/// Sends turns `1..=count` and returns their outcomes.
async fn send_turns(session: &mut Session, count: usize) -> Vec<TurnOutcome> {
    let mut outcomes = Vec::new();
    for number in 1..=count {
        outcomes.push(session.send(input(number)).await.expect("turn succeeds"));
    }

    outcomes
}

fn compactions(requests: &Mutex<Vec<ModelRequest>>) -> Vec<ModelRequest> {
    requests
        .lock()
        .expect("requests lock")
        .iter()
        .filter(|request| is_compaction(request))
        .cloned()
        .collect()
}

fn turn_requests(requests: &Mutex<Vec<ModelRequest>>) -> Vec<ModelRequest> {
    requests
        .lock()
        .expect("requests lock")
        .iter()
        .filter(|request| !is_compaction(request))
        .cloned()
        .collect()
}

fn last_turn_request(requests: &Mutex<Vec<ModelRequest>>) -> ModelRequest {
    turn_requests(requests).pop().expect("turn request")
}

fn carries(request: &ModelRequest, text: &str) -> bool {
    request.messages().iter().any(|message| match message {
        ModelMessage::User(content) => content.contains(text),
        _ => false,
    })
}

fn request_purposes(requests: &[ModelRequestActivity]) -> Vec<ModelRequestPurpose> {
    requests.iter().map(ModelRequestActivity::purpose).collect()
}

fn purposes(outcome: &TurnOutcome) -> Vec<ModelRequestPurpose> {
    request_purposes(outcome.report().model_requests())
}

/// Streams one run of `input` and returns its event kinds with its outcome.
async fn streamed_run(session: &Session, input: String) -> (Vec<RunEventKind>, RunOutcome) {
    streamed_request(session, RunRequest::new(input)).await
}

/// Streams one run of `request` and returns its event kinds with its
/// outcome.
async fn streamed_request(
    session: &Session,
    request: RunRequest,
) -> (Vec<RunEventKind>, RunOutcome) {
    let (mut stream, handle) = session.run(request).into_parts();
    let drain = async {
        let mut events = Vec::new();
        while let Some(event) =
            std::future::poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await
        {
            events.push(event.kind);
        }

        events
    };

    tokio::join!(drain, handle.outcome())
}

/// Streams one run of turn `number` and returns its event kinds with its
/// outcome.
async fn streamed_turn(session: &Session, number: usize) -> (Vec<RunEventKind>, RunOutcome) {
    streamed_run(session, input(number)).await
}

fn compaction_report(events: &[RunEventKind]) -> CompactionReport {
    events
        .iter()
        .find_map(|kind| match kind {
            RunEventKind::ContextCompacted(report) => Some(report.clone()),
            _ => None,
        })
        .expect("compaction event")
}

fn streamed_input_tokens(events: &[RunEventKind]) -> u64 {
    events
        .iter()
        .filter_map(|kind| match kind {
            RunEventKind::ModelRequestCompleted { usage, .. } => Some(usage.input),
            _ => None,
        })
        .sum()
}

fn evicted(outcome: &RunOutcome) -> u64 {
    u64::try_from(outcome.report.history().evicted_turns()).expect("count fits")
}

/// Returns the failure a session reports for a turn whose model fails with
/// `error`.
fn model_failure(error: ModelError) -> RunFailure {
    RunFailure::from(&RunError::from(SessionError::from(TurnError::from(error))))
}

#[tokio::test]
async fn history_below_the_threshold_is_never_compacted() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let harness = harness(model, at(100));
    let mut session = harness
        .session("below", object_schema())
        .create()
        .await
        .expect("session created");

    // Act
    let outcomes = send_turns(&mut session, 3).await;

    // Assert
    assert_eq!(compactions(&requests).len(), 0);
    assert!(session.checkpoint().is_none());
    for outcome in &outcomes {
        assert_eq!(purposes(outcome), [ModelRequestPurpose::Turn]);
        assert_eq!(outcome.report().history().evicted_turns(), 0);
    }
}

#[tokio::test]
async fn the_evict_policy_evicts_history_without_compacting() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let harness = harness(model, ContextPolicy::default());
    let mut session = harness
        .session("evicting", object_schema())
        .create()
        .await
        .expect("session created");

    // Act
    let outcomes = send_turns(&mut session, 4).await;

    // Assert
    assert_eq!(compactions(&requests).len(), 0);
    let last = outcomes.last().expect("last outcome");
    assert_eq!(purposes(last), [ModelRequestPurpose::Turn]);
    assert!(last.report().history().evicted_turns() > 0);
    assert!(!carries(&last_turn_request(&requests), "turn-1 "));
}

#[tokio::test]
async fn auto_compaction_publishes_a_durable_checkpoint_before_admitting_the_turn() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let harness = harness(model, ContextPolicy::auto_compact());
    let mut session = harness
        .session("compacting", object_schema())
        .create()
        .await
        .expect("session created");

    // Act
    let outcomes = send_turns(&mut session, 4).await;
    let resumed = harness.resume("compacting").await.expect("session resumes");

    // Assert
    let compactions = compactions(&requests);
    assert_eq!(compactions.len(), 1);
    // The newest finished turn stays uncovered and replays in full.
    assert!(carries(&compactions[0], "turn-2 "));
    assert!(!carries(&compactions[0], "turn-3 "));
    let last = outcomes.last().expect("last outcome");
    assert_eq!(
        purposes(last),
        [ModelRequestPurpose::Compaction, ModelRequestPurpose::Turn]
    );
    assert_eq!(last.report().history().evicted_turns(), 0);
    assert!(last.report().history().checkpoint_replayed());
    let request = last_turn_request(&requests);
    assert!(carries(&request, "cobalt-41"));
    assert!(carries(&request, "turn-3 "));
    assert!(!carries(&request, "turn-1 "));
    let checkpoint = resumed.checkpoint().expect("durable checkpoint");
    assert_eq!(checkpoint.covered_through(), 1);
    assert_eq!(session.checkpoint(), Some(checkpoint));
}

#[tokio::test]
async fn lower_thresholds_compact_earlier() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let harness = harness(model, at(50));
    let mut session = harness
        .session("early", object_schema())
        .create()
        .await
        .expect("session created");

    // Act
    let outcomes = send_turns(&mut session, 3).await;

    // Assert
    assert_eq!(compactions(&requests).len(), 1);
    assert_eq!(purposes(&outcomes[1]), [ModelRequestPurpose::Turn]);
    assert_eq!(
        purposes(&outcomes[2]),
        [ModelRequestPurpose::Compaction, ModelRequestPurpose::Turn]
    );
    assert_eq!(
        session.checkpoint().map(SessionCheckpoint::covered_through),
        Some(0)
    );
}

#[tokio::test]
async fn a_compaction_source_cut_short_by_size_covers_every_turn_it_holds() {
    // Arrange
    let store = Arc::new(MemoryStore::new());
    // Two full turns fit the byte budget, so after five uncompacted turns
    // the store's oldest uncovered window ends before the newest loaded one.
    let max_history_bytes = NonZeroUsize::new(2 * INPUT_BYTES + 400).expect("nonzero bytes");
    let (evicting, _) = CompactingModel::new(Vec::new());
    let mut session = harness(evicting, ContextPolicy::Evict)
        .store(Arc::clone(&store) as Arc<dyn crate::store::SessionStore>)
        .max_history_bytes(max_history_bytes)
        .session("windowed", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 5).await;
    let (model, requests) = CompactingModel::new(Vec::new());
    // Any replayed history is due to compact at this threshold.
    let harness = harness(model, at(1))
        .store(store)
        .max_history_bytes(max_history_bytes);
    let mut session = harness.resume("windowed").await.expect("session resumes");

    // Act
    let outcome = session.send(input(6)).await.expect("turn 6 succeeds");

    // Assert
    let compactions = compactions(&requests);
    assert_eq!(compactions.len(), 1);
    assert!(carries(&compactions[0], "turn-1 "));
    assert!(carries(&compactions[0], "turn-2 "));
    assert!(!carries(&compactions[0], "turn-3 "));
    // Turn 2 is covered although it is the newest turn the source holds,
    // because the newest loaded turn, turn 5, stays uncovered anyway.
    assert_eq!(
        session.checkpoint().map(SessionCheckpoint::covered_through),
        Some(1)
    );
    assert_eq!(
        purposes(&outcome),
        [ModelRequestPurpose::Compaction, ModelRequestPurpose::Turn]
    );
    assert!(carries(&last_turn_request(&requests), "turn-5 "));
}

#[tokio::test]
async fn failed_compaction_falls_back_to_eviction_and_reports_it() {
    // Arrange
    let (model, requests) = CompactingModel::new(vec![Err(ModelError::InvalidResponse)]);
    let harness = harness(model, ContextPolicy::auto_compact());
    let mut session = harness
        .session("failing", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;

    // Act
    let (events, outcome) = streamed_turn(&session, 4).await;

    // Assert
    assert!(outcome.result.is_ok());
    assert_eq!(compactions(&requests).len(), 1);
    let report = compaction_report(&events);
    assert_eq!(
        report.outcome,
        CompactionOutcome::Failed(model_failure(ModelError::InvalidResponse))
    );
    assert!(report.evicted_turns > 0);
    assert_eq!(report.evicted_turns, evicted(&outcome));
    // The failed request completed nothing, so only the turn's own request
    // is reported and streamed.
    assert_eq!(
        request_purposes(outcome.report.model_requests()),
        [ModelRequestPurpose::Turn]
    );
    assert_eq!(outcome.usage.input, 0);
    assert_eq!(streamed_input_tokens(&events), 0);
    assert!(!carries(&last_turn_request(&requests), "turn-1 "));
    assert!(
        harness
            .resume("failing")
            .await
            .expect("session resumes")
            .checkpoint()
            .is_none()
    );
}

#[tokio::test]
async fn a_rejected_summary_falls_back_to_eviction_and_still_counts_its_usage() {
    // Arrange
    let (model, requests) =
        CompactingModel::new(vec![Ok(ModelResponse::Output(oversized_summary()))]);
    let harness = harness(model, ContextPolicy::auto_compact());
    let mut session = harness
        .session("rejected", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;

    // Act
    let (events, outcome) = streamed_turn(&session, 4).await;

    // Assert
    assert!(outcome.result.is_ok());
    assert_eq!(compactions(&requests).len(), 1);
    let report = compaction_report(&events);
    // Sessions classify a rejected checkpoint summary as a store failure.
    assert!(matches!(
        &report.outcome,
        CompactionOutcome::Failed(RunFailure { kind: ErrorKind::Store, message, retryable: false })
            if message.contains("at most 16384")
    ));
    assert!(report.evicted_turns > 0);
    assert_eq!(
        request_purposes(outcome.report.model_requests()),
        [ModelRequestPurpose::Compaction, ModelRequestPurpose::Turn]
    );
    assert_eq!(outcome.usage.input, COMPACTION_INPUT_TOKENS);
    assert!(matches!(
        &events[1],
        RunEventKind::ModelRequestCompleted { usage, .. }
            if usage.input == COMPACTION_INPUT_TOKENS
    ));
    assert!(matches!(events[2], RunEventKind::ContextCompacted(_)));
    assert!(session.checkpoint().is_none());
}

#[tokio::test]
async fn compaction_that_still_does_not_fit_evicts_after_the_new_checkpoint() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let harness = harness(model, ContextPolicy::auto_compact());
    let mut session = harness
        .session("still-evicting", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;

    // Act
    // The newest finished turn stays uncovered, and beside this input it
    // no longer fits the weight left for history.
    let (events, outcome) = streamed_run(&session, sized_input(4, 3 * INPUT_BYTES)).await;

    // Assert
    assert!(outcome.result.is_ok());
    let report = compaction_report(&events);
    assert_eq!(
        report.outcome,
        CompactionOutcome::Compacted {
            covered_through: TurnIndex::from_position(1),
            model: None,
        }
    );
    assert!(report.evicted_turns > 0);
    assert_eq!(report.evicted_turns, evicted(&outcome));
    assert!(outcome.report.history().checkpoint_replayed());
    let request = last_turn_request(&requests);
    assert!(carries(&request, "cobalt-41"));
    assert!(!carries(&request, "turn-3 "));
    assert_eq!(
        harness
            .resume("still-evicting")
            .await
            .expect("session resumes")
            .checkpoint()
            .map(SessionCheckpoint::covered_through),
        Some(1)
    );
}

#[tokio::test]
async fn a_failed_turn_after_compaction_still_reports_the_compaction() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let harness = harness(model.failing_turns_after(3), ContextPolicy::auto_compact());
    let mut session = harness
        .session("failed-turn", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;

    // Act
    let (events, outcome) = streamed_turn(&session, 4).await;

    // Assert
    assert!(outcome.result.is_err());
    assert_eq!(compactions(&requests).len(), 1);
    assert_eq!(
        outcome
            .report
            .model_requests()
            .first()
            .map(ModelRequestActivity::purpose),
        Some(ModelRequestPurpose::Compaction)
    );
    assert_eq!(outcome.usage.input, COMPACTION_INPUT_TOKENS);
    let compacted = events
        .iter()
        .position(|kind| matches!(kind, RunEventKind::ContextCompacted(_)))
        .expect("compaction event");
    assert!(matches!(
        events.last(),
        Some(RunEventKind::Completed(summary)) if summary.error.is_some()
    ));
    assert!(compacted < events.len() - 1);
}

#[tokio::test]
async fn failed_compaction_backs_off_before_retrying() {
    // Arrange
    let (model, requests) = CompactingModel::new(vec![
        Err(ModelError::InvalidResponse),
        Err(ModelError::InvalidResponse),
    ]);
    // At this threshold every turn after the second is due to compact.
    let harness = harness(model, at(50));
    let mut session = harness
        .session("backing-off", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 2).await;
    let mut attempts = Vec::new();

    // Act
    for number in 3..=8 {
        session.send(input(number)).await.expect("turn succeeds");
        attempts.push(compactions(&requests).len());
    }

    // Assert
    // Turn 3 fails, turn 4 is skipped, turn 5 fails again, turns 6 and 7
    // are skipped, and turn 8 compacts.
    assert_eq!(attempts, [1, 1, 2, 2, 2, 3]);
    assert!(session.checkpoint().is_some());
}

#[tokio::test]
async fn streamed_compaction_runs_on_the_summary_model_and_counts_its_usage() {
    // Arrange
    let (smart, smart_requests) = CompactingModel::new(Vec::new());
    let (cheap, cheap_requests) = CompactingModel::new(Vec::new());
    let catalog = ModelCatalog::builder()
        .model(
            ModelSpec::new("host/smart").with_profile(ModelProfile::new(BUDGET)),
            Arc::new(smart.named("smart")),
        )
        .model(
            ModelSpec::new("host/cheap")
                .with_capabilities(Capabilities::NONE.with_structured_output(true)),
            Arc::new(cheap.named("cheap")),
        )
        .assign(Role::SMART, ["host/smart"])
        .assign(Role::CHEAP, ["host/cheap"])
        .build()
        .expect("catalog builds");
    let harness = Harness::builder()
        .catalog(Arc::new(catalog))
        .store(Arc::new(MemoryStore::new()))
        .context_policy(ContextPolicy::auto_compact())
        .build()
        .expect("harness builds");
    let mut session = harness
        .session("cataloged", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;

    // Act
    let (events, outcome) = streamed_turn(&session, 4).await;

    // Assert
    assert!(outcome.result.is_ok());
    assert_eq!(compactions(&smart_requests).len(), 0);
    assert_eq!(compactions(&cheap_requests).len(), 1);
    assert!(matches!(events[0], RunEventKind::Started { .. }));
    assert!(matches!(
        &events[1],
        RunEventKind::ModelRequestCompleted { model: Some(model), usage, .. }
            if model.as_str() == "host/cheap" && usage.input == COMPACTION_INPUT_TOKENS
    ));
    let report = compaction_report(&events);
    assert!(matches!(events[2], RunEventKind::ContextCompacted(_)));
    assert_eq!(
        report.outcome,
        CompactionOutcome::Compacted {
            covered_through: TurnIndex::from_position(1),
            model: Some(ModelId::new("host/cheap")),
        }
    );
    assert_eq!(report.evicted_turns, 0);
    assert_eq!(streamed_input_tokens(&events), outcome.usage.input);
    assert_eq!(outcome.usage.input, COMPACTION_INPUT_TOKENS);
    assert_eq!(
        outcome.report.model_requests()[0].purpose(),
        ModelRequestPurpose::Compaction
    );
    let resumed = harness.resume("cataloged").await.expect("session resumes");
    assert_eq!(
        resumed
            .checkpoint()
            .and_then(|checkpoint| checkpoint.model()),
        Some("cheap")
    );
}

#[tokio::test]
async fn compaction_requests_belong_to_the_turns_own_lifecycle_turn() {
    // Arrange
    let recorder = LifecycleRecorder::default();
    let (model, _requests) = CompactingModel::new(Vec::new());
    let harness =
        harness(model, ContextPolicy::auto_compact()).with_lifecycle_observer(recorder.clone());
    let mut session = harness
        .session("traced", object_schema())
        .create()
        .await
        .expect("session created");

    // Act
    let outcomes = send_turns(&mut session, 4).await;

    // Assert
    assert_eq!(
        purposes(outcomes.last().expect("last outcome")),
        [ModelRequestPurpose::Compaction, ModelRequestPurpose::Turn]
    );
    let events = recorder.events.lock().expect("events lock").clone();
    let turns: Vec<_> = events
        .iter()
        .filter_map(|event| match event.kind() {
            LifecycleEventKind::TurnStarted { turn_id } => Some(*turn_id),
            _ => None,
        })
        .collect();
    assert_eq!(turns.len(), 4);
    let last_turn_requests: Vec<u64> = events
        .iter()
        .filter_map(|event| match event.kind() {
            LifecycleEventKind::ModelRequestStarted {
                request_index,
                turn_id: Some(turn_id),
                ..
            } if *turn_id == turns[3] => Some(*request_index),
            _ => None,
        })
        .collect();
    assert_eq!(last_turn_requests, [0, 1]);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind(), LifecycleEventKind::TurnFailed { .. }))
    );
}

#[tokio::test]
async fn an_empty_compaction_source_reports_nothing_to_cover() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let harness = harness(model, ContextPolicy::auto_compact());
    let mut session = harness
        .session("covered", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 2).await;
    // Another handle covers every finished turn after this handle loaded
    // its history.
    harness
        .resume("covered")
        .await
        .expect("session resumes")
        .compact()
        .await
        .expect("compaction succeeds");
    let compacted = compactions(&requests).len();

    // Act
    let compaction = session
        .auto_compact(
            EnclosingTurn {
                control: None,
                lifecycle: None,
                limits: RunLimits::default(),
            },
            2,
            1,
        )
        .await
        .expect("compaction due");

    // Assert
    assert_eq!(compaction.outcome, CompactionOutcome::NothingToCover);
    assert_eq!(compaction.requests.activities.len(), 0);
    assert_eq!(compactions(&requests).len(), compacted);
    assert_eq!(
        session.checkpoint().map(SessionCheckpoint::covered_through),
        Some(1)
    );
    assert_eq!(session.history.turns().len(), 0);
}

#[tokio::test]
async fn cancelling_a_run_during_compaction_keeps_its_usage_and_the_session() {
    // Arrange
    let started = Arc::new(Notify::new());
    // The first answer violates the checkpoint schema, so generation sends
    // a correction, which stalls.
    let (model, requests) =
        CompactingModel::new(vec![Ok(ModelResponse::Output(json!({"unexpected": true})))]);
    let harness = harness(
        model.stalling_compaction(1, Arc::clone(&started)),
        ContextPolicy::auto_compact(),
    );
    let mut session = harness
        .session("cancelled", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;
    let handle = session.run(RunRequest::new(input(4))).into_handle();
    let cancel = handle.cancel_token();

    // Act
    let (outcome, ()) = tokio::join!(handle.outcome(), async {
        wait_for_fixture(&started, "the compaction correction").await;
        cancel.cancel();
    });
    let turn_4_sent = turn_requests(&requests)
        .iter()
        .any(|request| carries(request, "turn-4 "));
    let resumed = harness.resume("cancelled").await.expect("session resumes");
    let stored = session.usage().await.expect("stored usage loads");
    let follow_up = session.send(input(5)).await;

    // Assert
    let error = outcome.result.expect_err("cancelled run");
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert_eq!(
        request_purposes(outcome.report.model_requests()),
        [ModelRequestPurpose::Compaction]
    );
    assert_eq!(outcome.usage.input, COMPACTION_INPUT_TOKENS);
    // The stopped turn's stored usage keeps the billed compaction request.
    assert_eq!(stored.usage.input, COMPACTION_INPUT_TOKENS);
    assert!(!turn_4_sent);
    assert!(resumed.checkpoint().is_none());
    let follow_up = follow_up.expect("the session accepts the next turn");
    assert_eq!(
        purposes(&follow_up),
        [ModelRequestPurpose::Compaction, ModelRequestPurpose::Turn]
    );
}

#[tokio::test]
async fn losing_the_lease_during_compaction_interrupts_the_turn() {
    // Arrange
    let store = Arc::new(FaultStore::new());
    let started = Arc::new(Notify::new());
    let (model, _requests) = CompactingModel::new(Vec::new());
    let harness = harness(
        model.stalling_compaction(0, Arc::clone(&started)),
        ContextPolicy::auto_compact(),
    )
    .store(Arc::clone(&store) as Arc<dyn SessionStore>);
    let mut session = harness
        .session("leased", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;
    store.inject(Fault::Renew);

    // Act
    let (result, ()) = tokio::join!(session.send(input(4)), async {
        wait_for_fixture(&started, "the compaction request").await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(TURN_LEASE_RENEWAL_INTERVAL_SECONDS)).await;
        tokio::time::resume();
    });

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::Store {
            operation: "renew turn lease",
            ..
        })
    ));
}

#[tokio::test]
async fn compaction_requests_count_toward_the_runs_request_limit() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let mut session = harness(model, ContextPolicy::auto_compact())
        .session("limited", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;
    let turns_before = turn_requests(&requests).len();
    let limits = RunLimits::default().with_max_model_requests(NonZeroU32::MIN);

    // Act
    let (_events, outcome) =
        streamed_request(&session, RunRequest::new(input(4)).limits(limits)).await;

    // Assert
    assert_eq!(
        outcome.result.as_ref().err().map(RunError::kind),
        Some(ErrorKind::BudgetExhausted(LimitKind::ModelRequests))
    );
    assert_eq!(compactions(&requests).len(), 1);
    assert_eq!(turn_requests(&requests).len(), turns_before);
    assert_eq!(
        request_purposes(outcome.report.model_requests()),
        [ModelRequestPurpose::Compaction]
    );
    assert_eq!(outcome.usage.input, COMPACTION_INPUT_TOKENS);
}

#[tokio::test]
async fn compaction_requests_reserve_from_the_runs_budget_ledger() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let mut session = harness(model, ContextPolicy::auto_compact())
        .session("ledgered", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;
    let turns_before = turn_requests(&requests).len();
    let ledger = Arc::new(CountingLedger {
        remaining: Mutex::new(1),
    });

    // Act
    let (_events, outcome) =
        streamed_request(&session, RunRequest::new(input(4)).budget(ledger)).await;

    // Assert
    // The compaction request takes the ledger's only reservation, so the
    // turn's own first request is refused at the ledger's cap.
    assert_eq!(
        outcome.result.as_ref().err().map(RunError::kind),
        Some(ErrorKind::BudgetExhausted(LimitKind::Cost))
    );
    assert_eq!(compactions(&requests).len(), 1);
    assert_eq!(turn_requests(&requests).len(), turns_before);
    assert_eq!(
        request_purposes(outcome.report.model_requests()),
        [ModelRequestPurpose::Compaction]
    );
}

#[tokio::test(start_paused = true)]
async fn the_wall_time_limit_runs_from_the_start_of_compaction() {
    // Arrange
    let (model, requests) = CompactingModel::new(Vec::new());
    let mut session = harness(
        model.delayed(Duration::from_secs(40)),
        ContextPolicy::auto_compact(),
    )
    .session("timed", object_schema())
    .create()
    .await
    .expect("session created");
    send_turns(&mut session, 3).await;
    let limits = RunLimits::default().with_max_wall_time(Duration::from_secs(60));

    // Act
    let (_events, outcome) =
        streamed_request(&session, RunRequest::new(input(4)).limits(limits)).await;

    // Assert
    // Compaction and the turn's request each fit the limit alone, but not
    // together.
    assert_eq!(
        outcome.result.as_ref().err().map(RunError::kind),
        Some(ErrorKind::BudgetExhausted(LimitKind::WallTime))
    );
    assert_eq!(compactions(&requests).len(), 1);
    assert_eq!(
        request_purposes(outcome.report.model_requests()),
        [ModelRequestPurpose::Compaction]
    );
}

/// Ledger that admits a fixed number of model requests and refuses every
/// later one at its cost cap.
struct CountingLedger {
    remaining: Mutex<usize>,
}

#[async_trait]
impl BudgetLedger for CountingLedger {
    async fn reserve(&self, estimate: Estimate) -> Result<Reservation, LimitKind> {
        let mut remaining = self.remaining.lock().expect("remaining lock");
        *remaining = remaining.checked_sub(1).ok_or(LimitKind::Cost)?;

        Ok(Reservation::new(estimate, drop))
    }

    fn commit(&self, _reservation: Reservation, _actual: &Usage, _cost: Option<Cost>) {}
}
