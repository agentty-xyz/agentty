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

use super::support::{LifecycleRecorder, object_schema, wait_for_fixture};
use crate::budget::{BudgetLedger, Estimate, Reservation};
use crate::catalog::{
    Capabilities, ModelCatalog, ModelHandle, ModelId, ModelSelector, ModelSpec, Pricing, Role,
};
use crate::compaction::{GENERATION_INSTRUCTIONS, SessionCheckpoint};
use crate::context::{ContextBudget, ContextPolicy};
use crate::error::{ErrorKind, RunError};
use crate::event::{CompactionOutcome, CompactionReport, RunEventKind, RunFailure, TurnIndex};
use crate::fault_store_test::{Fault, FaultStore};
use crate::harness::{CompactionBackoff, CompactionRequests, EnclosingTurn, Harness, Session};
use crate::lifecycle::LifecycleEventKind;
use crate::limit::{LimitKind, RunLimits};
use crate::model::{
    CompletionMetadata, CompletionUsage, Model, ModelCompletion, ModelError, ModelMessage,
    ModelMetadata, ModelProfile, ModelRegistry, ModelRequest, ModelResponse,
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

/// Scripted answer to one compaction request.
type ScriptedCompaction = Result<ModelResponse, ModelError>;

/// Answers turn requests with a short output and compaction requests with
/// the scripted responses, in order, then with a valid summary.
struct CompactingModel {
    compactions: Mutex<VecDeque<ScriptedCompaction>>,
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
        let usage = CompletionUsage::default()
            .with_input_tokens(Some(COMPACTION_INPUT_TOKENS))
            .with_output_tokens(Some(20));
        let metadata = CompletionMetadata::new("stop").with_usage(Some(usage));

        Ok(ModelCompletion::new(metadata, response))
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

/// Returns an automatically compacting harness whose only model, `model`,
/// is cataloged at one micro-USD per input token.
fn priced_harness(model: CompactingModel) -> Harness {
    let catalog = ModelCatalog::builder()
        .model(
            ModelSpec::new("host/priced")
                .with_profile(ModelProfile::new(BUDGET))
                .with_pricing(Pricing::new(1_000_000, 0)),
            Arc::new(model),
        )
        .assign(Role::SMART, ["host/priced"])
        .build()
        .expect("catalog builds");

    Harness::builder()
        .catalog(Arc::new(catalog))
        .store(Arc::new(MemoryStore::new()))
        .context_policy(ContextPolicy::auto_compact())
        // Loads as many turns as the uncataloged harness does.
        .max_history_bytes(NonZeroUsize::new(1 << 20).expect("nonzero bytes"))
        .build()
        .expect("harness builds")
}

/// Streams one run of turn 4 and cancels it once `started` is notified,
/// returning its event kinds with its outcome.
async fn cancelled_when(
    session: &Session,
    started: &Notify,
    description: &str,
) -> (Vec<RunEventKind>, RunOutcome) {
    let (mut stream, handle) = session.run(RunRequest::new(input(4))).into_parts();
    let cancel = handle.cancel_token();
    let drain = async {
        let mut events = Vec::new();
        while let Some(event) =
            std::future::poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await
        {
            events.push(event.kind);
        }

        events
    };
    let (events, outcome, ()) = tokio::join!(drain, handle.outcome(), async {
        wait_for_fixture(started, description).await;
        cancel.cancel();
    });

    (events, outcome)
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
            RunEventKind::Compacted(report) => Some(report.clone()),
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
    assert!(evicted(&outcome) > 0);
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
    assert!(evicted(&outcome) > 0);
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
    assert!(matches!(events[2], RunEventKind::Compacted(_)));
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
    assert!(evicted(&outcome) > 0);
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
        .position(|kind| matches!(kind, RunEventKind::Compacted(_)))
        .expect("compaction event");
    assert!(matches!(
        events.last(),
        Some(RunEventKind::Completed(summary)) if summary.error.is_some()
    ));
    assert!(compacted < events.len() - 1);
}

#[tokio::test]
async fn failed_compaction_backs_off_before_retrying() {
    // Streamed runs execute on copies of the handle, which share its backoff.
    for streamed in [false, true] {
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
            if streamed {
                let (_events, outcome) = streamed_turn(&session, number).await;
                assert!(outcome.result.is_ok());
            } else {
                session.send(input(number)).await.expect("turn succeeds");
            }
            attempts.push(compactions(&requests).len());
        }

        // Assert
        // Turn 3 fails, turn 4 is skipped, turn 5 fails again, turns 6 and 7
        // are skipped, and turn 8 compacts.
        assert_eq!(attempts, [1, 1, 2, 2, 2, 3], "streamed: {streamed}");
    }
}

#[test]
fn the_backoff_doubles_up_to_32_skipped_turns_and_resets_after_a_success() {
    // Arrange
    let mut backoff = CompactionBackoff::default();
    let mut retry_at = Vec::new();

    // Act
    for _ in 0..8 {
        backoff.failed(10);
        retry_at.push(backoff.retry_at);
    }
    backoff = CompactionBackoff::default();
    backoff.failed(10);

    // Assert
    assert_eq!(retry_at, [12, 13, 15, 19, 27, 43, 43, 43]);
    assert_eq!(backoff.retry_at, 12);
}

#[tokio::test]
async fn streamed_compaction_runs_on_the_summary_model_and_counts_its_usage() {
    // Arrange
    let (smart, smart_requests) = CompactingModel::new(Vec::new());
    let (cheap, cheap_requests) = CompactingModel::new(Vec::new());
    let cheap_pricing = Pricing::new(3_000_000, 0);
    let catalog = ModelCatalog::builder()
        .model(
            ModelSpec::new("host/smart")
                .with_profile(ModelProfile::new(BUDGET))
                .with_pricing(Pricing::new(1_000_000, 0)),
            Arc::new(smart.named("smart")),
        )
        .model(
            ModelSpec::new("host/cheap")
                .with_capabilities(Capabilities::NONE.with_structured_output(true))
                .with_pricing(cheap_pricing),
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
    // Priced at the summary model: 700 input tokens at 3 micro-USD each.
    let compaction_cost = Some(Cost::of(
        &Usage {
            input: COMPACTION_INPUT_TOKENS,
            output: 20,
            ..Usage::default()
        },
        cheap_pricing,
    ));
    assert_eq!(compaction_cost.map(|cost| cost.micro_usd), Some(2_100));
    assert!(matches!(
        &events[1],
        RunEventKind::ModelRequestCompleted { cost, model: Some(model), usage, .. }
            if model.as_str() == "host/cheap"
                && usage.input == COMPACTION_INPUT_TOKENS
                && *cost == compaction_cost
    ));
    let report = compaction_report(&events);
    assert!(matches!(events[2], RunEventKind::Compacted(_)));
    assert_eq!(
        report.outcome,
        CompactionOutcome::Compacted {
            covered_through: TurnIndex::from_position(1),
            model: Some(ModelId::new("host/cheap")),
        }
    );
    assert_eq!(outcome.report.history().evicted_turns(), 0);
    assert_eq!(streamed_input_tokens(&events), outcome.usage.input);
    assert_eq!(
        outcome.cost.map(|cost| cost.micro_usd),
        compaction_cost.map(|cost| cost.micro_usd)
    );
    assert!(matches!(
        events.last(),
        Some(RunEventKind::Completed(summary)) if summary.cost == outcome.cost
    ));
    assert_eq!(
        session.usage().await.expect("stored usage loads").cost,
        outcome.cost
    );
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
    // A compaction that succeeds, one whose request fails, and one whose
    // first answer violates the checkpoint schema and is corrected.
    let cases: [(Vec<ScriptedCompaction>, &[u64]); 3] = [
        (Vec::new(), &[0, 1]),
        (vec![Err(ModelError::InvalidResponse)], &[0, 1]),
        (
            vec![Ok(ModelResponse::Output(json!({"unexpected": true})))],
            &[0, 1, 2],
        ),
    ];
    for (compactions, expected_indexes) in cases {
        // Arrange
        let recorder = LifecycleRecorder::default();
        let (model, _requests) = CompactingModel::new(compactions);
        let harness =
            harness(model, ContextPolicy::auto_compact()).with_lifecycle_observer(recorder.clone());
        let mut session = harness
            .session("traced", object_schema())
            .create()
            .await
            .expect("session created");

        // Act
        send_turns(&mut session, 4).await;

        // Assert
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
        assert_eq!(last_turn_requests, expected_indexes);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.kind(), LifecycleEventKind::TurnFailed { .. }))
        );
    }
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

    let mut completed = CompactionRequests::default();

    // Act
    let outcome = session
        .auto_compact(
            EnclosingTurn {
                control: None,
                lifecycle: None,
                limits: RunLimits::default(),
            },
            2,
            1,
            &mut completed,
        )
        .await
        .expect("compaction does not fail the turn")
        .expect("compaction due");

    // Assert
    assert_eq!(outcome, CompactionOutcome::NothingToCover);
    assert_eq!(completed.activities.len(), 0);
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
    let harness = priced_harness(model.stalling_compaction(1, Arc::clone(&started)));
    let mut session = harness
        .session("cancelled", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;

    // Act
    let (events, outcome) = cancelled_when(&session, &started, "the compaction correction").await;
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
    // The billed request streamed as it completed, so the events sum to the
    // run's usage although compaction never finished.
    assert_eq!(streamed_input_tokens(&events), COMPACTION_INPUT_TOKENS);
    assert!(matches!(
        events.last(),
        Some(RunEventKind::Completed(summary)) if summary.usage == outcome.usage
    ));
    assert!(
        !events
            .iter()
            .any(|kind| matches!(kind, RunEventKind::Compacted(_)))
    );
    // The stopped turn's stored usage keeps the billed compaction request.
    assert_eq!(stored.usage.input, COMPACTION_INPUT_TOKENS);
    assert_eq!(outcome.cost.map(|cost| cost.micro_usd), Some(700));
    assert_eq!(stored.cost, outcome.cost);
    assert!(!turn_4_sent);
    assert!(resumed.checkpoint().is_none());
    let follow_up = follow_up.expect("the session accepts the next turn");
    assert_eq!(
        purposes(&follow_up),
        [ModelRequestPurpose::Compaction, ModelRequestPurpose::Turn]
    );
}

#[tokio::test]
async fn cancelling_a_run_while_its_checkpoint_publishes_keeps_its_usage() {
    // Arrange
    let store = Arc::new(FaultStore::new());
    let publishing = Arc::new(Notify::new());
    let (model, _requests) = CompactingModel::new(Vec::new());
    let harness = harness(model, ContextPolicy::auto_compact())
        .store(Arc::clone(&store) as Arc<dyn SessionStore>);
    let mut session = harness
        .session("publishing", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;
    store.stall_publication(Arc::clone(&publishing));

    // Act
    let (events, outcome) =
        cancelled_when(&session, &publishing, "the checkpoint publication").await;
    let stored = session.usage().await.expect("stored usage loads");

    // Assert
    assert_eq!(
        outcome.result.as_ref().err().map(RunError::kind),
        Some(ErrorKind::Cancelled)
    );
    assert_eq!(
        request_purposes(outcome.report.model_requests()),
        [ModelRequestPurpose::Compaction]
    );
    assert_eq!(outcome.usage.input, COMPACTION_INPUT_TOKENS);
    assert_eq!(streamed_input_tokens(&events), COMPACTION_INPUT_TOKENS);
    assert_eq!(stored.usage.input, COMPACTION_INPUT_TOKENS);
    assert!(session.checkpoint().is_none());
}

#[tokio::test]
async fn losing_the_lease_during_compaction_interrupts_the_turn_and_reports_its_usage() {
    // Arrange
    let store = Arc::new(FaultStore::new());
    let started = Arc::new(Notify::new());
    // The first answer violates the checkpoint schema, so generation sends
    // a correction, which stalls after one request was billed.
    let (model, requests) =
        CompactingModel::new(vec![Ok(ModelResponse::Output(json!({"unexpected": true})))]);
    let harness = harness(
        model.stalling_compaction(1, Arc::clone(&started)),
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
    let (outcome, ()) = tokio::join!(session.run(RunRequest::new(input(4))), async {
        wait_for_fixture(&started, "the compaction correction").await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(TURN_LEASE_RENEWAL_INTERVAL_SECONDS)).await;
        tokio::time::resume();
    });
    store.repair(Fault::Renew);
    let turn_4_sent = turn_requests(&requests)
        .iter()
        .any(|request| carries(request, "turn-4 "));
    let stored = session.usage().await.expect("stored usage loads");
    let resumed = harness.resume("leased").await.expect("session resumes");

    // Assert
    assert_eq!(
        outcome.result.as_ref().err().map(RunError::kind),
        Some(ErrorKind::Store)
    );
    assert_eq!(
        request_purposes(outcome.report.model_requests()),
        [ModelRequestPurpose::Compaction]
    );
    assert_eq!(outcome.usage.input, COMPACTION_INPUT_TOKENS);
    // Only renewals failed, so the store still accepted the usage; a store
    // whose lease another owner took refuses it, and only the outcome then
    // counts the billed request.
    assert_eq!(stored.usage.input, COMPACTION_INPUT_TOKENS);
    assert!(!turn_4_sent);
    assert!(resumed.checkpoint().is_none());
}

#[tokio::test]
async fn compaction_spends_only_its_share_of_the_runs_limits() {
    // Arrange
    // Each compaction's first answer violates the checkpoint schema, so it
    // needs a correction that its share of the limits may not allow.
    let requests_limit = |max: u32| {
        RunLimits::default().with_max_model_requests(NonZeroU32::new(max).expect("nonzero"))
    };
    let tokens_limit =
        RunLimits::default().with_max_total_tokens(NonZeroU64::new(1_000).expect("nonzero tokens"));
    let cases = [
        // One request leaves compaction no share, so it is skipped.
        (requests_limit(1), 0, None),
        // Compaction may send one of two requests, so its correction stops.
        (requests_limit(2), 1, Some(LimitKind::ModelRequests)),
        // Compaction may use 500 of 1,000 tokens, and its first request
        // used 720, so its correction stops.
        (tokens_limit, 1, Some(LimitKind::Tokens)),
        // Three requests leave compaction two, enough for its correction.
        (requests_limit(3), 2, None),
    ];
    for (limits, compaction_requests, stopped_by) in cases {
        // Arrange
        let (model, requests) =
            CompactingModel::new(vec![Ok(ModelResponse::Output(json!({"unexpected": true})))]);
        let mut session = harness(model, ContextPolicy::auto_compact())
            .session("limited", object_schema())
            .create()
            .await
            .expect("session created");
        send_turns(&mut session, 3).await;

        // Act
        let (events, outcome) =
            streamed_request(&session, RunRequest::new(input(4)).limits(limits)).await;

        // Assert
        // The turn still answers within the run's limits.
        assert!(outcome.result.is_ok(), "{limits:?}");
        assert_eq!(compactions(&requests).len(), compaction_requests);
        assert!(carries(&last_turn_request(&requests), "turn-4 "));
        let reported = events.iter().find_map(|kind| match kind {
            RunEventKind::Compacted(report) => Some(report.outcome.clone()),
            _ => None,
        });
        match stopped_by {
            Some(limit) => assert!(matches!(
                reported,
                Some(CompactionOutcome::Failed(RunFailure {
                    kind: ErrorKind::BudgetExhausted(reached),
                    ..
                })) if reached == limit
            )),
            None if compaction_requests == 0 => assert_eq!(reported, None),
            None => assert!(matches!(
                reported,
                Some(CompactionOutcome::Compacted { .. })
            )),
        }
    }
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

#[tokio::test(start_paused = true)]
async fn run_failures_during_compaction_fail_the_turn_without_backing_off() {
    // The run's ledger refusing the compaction request, its wall-clock limit
    // passing during compaction, and a store failure publishing the
    // checkpoint.
    let refusing_ledger = Arc::new(CountingLedger {
        remaining: Mutex::new(0),
    });
    let cases = [
        (
            RunRequest::new(input(4)).budget(refusing_ledger),
            None,
            None,
            ErrorKind::BudgetExhausted(LimitKind::Cost),
        ),
        (
            RunRequest::new(input(4))
                .limits(RunLimits::default().with_max_wall_time(Duration::from_secs(60))),
            Some(Duration::from_secs(70)),
            None,
            ErrorKind::BudgetExhausted(LimitKind::WallTime),
        ),
        (
            RunRequest::new(input(4)),
            None,
            Some(Fault::PublishCheckpoint),
            ErrorKind::Store,
        ),
    ];
    for (request, delay, fault, kind) in cases {
        // Arrange
        let store = Arc::new(FaultStore::new());
        let (model, requests) = CompactingModel::new(Vec::new());
        let model = match delay {
            Some(delay) => model.delayed(delay),
            None => model,
        };
        let mut session = harness(model, ContextPolicy::auto_compact())
            .store(Arc::clone(&store) as Arc<dyn SessionStore>)
            .session("run-failure", object_schema())
            .create()
            .await
            .expect("session created");
        send_turns(&mut session, 3).await;
        let turns_before = turn_requests(&requests).len();
        if let Some(fault) = fault {
            store.inject(fault);
        }

        // Act
        let (events, outcome) = streamed_request(&session, request).await;
        if let Some(fault) = fault {
            store.repair(fault);
        }
        let attempts = compactions(&requests).len();
        let follow_up = session.send(input(5)).await;

        // Assert
        assert_eq!(
            outcome.result.as_ref().err().map(RunError::kind),
            Some(kind)
        );
        assert_eq!(turn_requests(&requests).len(), turns_before + 1);
        assert!(
            !events
                .iter()
                .any(|kind| matches!(kind, RunEventKind::Compacted(_)))
        );
        assert_eq!(streamed_input_tokens(&events), outcome.usage.input);
        // No backoff: the next turn compacts at once.
        assert!(follow_up.is_ok(), "{kind:?}");
        assert!(compactions(&requests).len() > attempts, "{kind:?}");
    }
}

/// Returns a catalog of unpriced session models `host/smart` and
/// `host/next`, a cheap model `host/cheap` priced at `cheap_pricing` that
/// declares structured output, and `host/plain`, which declares none.
fn registry_catalog(
    smart: CompactingModel,
    cheap: CompactingModel,
    cheap_pricing: Pricing,
) -> ModelCatalog {
    let (next, _) = CompactingModel::new(Vec::new());
    let (plain, _) = CompactingModel::new(Vec::new());

    ModelCatalog::builder()
        .model(
            ModelSpec::new("host/smart").with_profile(ModelProfile::new(BUDGET)),
            Arc::new(smart.named("smart")),
        )
        .model(
            ModelSpec::new("host/next").with_profile(ModelProfile::new(BUDGET)),
            Arc::new(next.named("next")),
        )
        .model(
            ModelSpec::new("host/cheap")
                .with_capabilities(Capabilities::NONE.with_structured_output(true))
                .with_pricing(cheap_pricing),
            Arc::new(cheap.named("cheap")),
        )
        .model(ModelSpec::new("host/plain"), Arc::new(plain.named("plain")))
        .build()
        .expect("catalog builds")
}

/// Returns the entry `id` of `catalog`.
fn entry(catalog: &ModelCatalog, id: &str) -> ModelHandle {
    catalog
        .resolve(&ModelSelector::Id(ModelId::new(id)))
        .expect("entry resolves")
}

/// Returns the registrations of `catalog`'s session models.
fn session_registry(catalog: &ModelCatalog) -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    for id in ["host/smart", "host/next"] {
        registry
            .register_catalog_entry(&entry(catalog, id), "host-1")
            .expect("entry registers");
    }

    registry
}

/// Returns an automatically compacting harness over the registration of
/// `catalog`'s session model, storing sessions in `store`.
fn registered_harness(catalog: &ModelCatalog, store: &Arc<MemoryStore>) -> Harness {
    Harness::from_registry(&session_registry(catalog), "host/smart")
        .expect("registration resolves")
        .store(Arc::clone(store) as Arc<dyn SessionStore>)
        .context_policy(ContextPolicy::auto_compact())
}

/// Returns [`registered_harness`] summarizing on `catalog`'s entry `id`.
fn summarizing_harness(catalog: &ModelCatalog, store: &Arc<MemoryStore>, id: &str) -> Harness {
    let mut harness = registered_harness(catalog, store);
    harness
        .set_summary_model(&entry(catalog, id))
        .expect("summary model accepted");

    harness
}

#[tokio::test]
async fn a_registered_harness_compacts_on_its_summary_model_at_its_prices() {
    // Arrange
    let (smart, smart_requests) = CompactingModel::new(Vec::new());
    let (cheap, cheap_requests) = CompactingModel::new(Vec::new());
    let cheap_pricing = Pricing::new(3_000_000, 0);
    let catalog = registry_catalog(smart, cheap, cheap_pricing);
    let store = Arc::new(MemoryStore::new());
    let harness = summarizing_harness(&catalog, &store, "host/cheap");
    let mut session = harness
        .session("registered", object_schema())
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
    assert_eq!(
        compaction_report(&events).outcome,
        CompactionOutcome::Compacted {
            covered_through: TurnIndex::from_position(1),
            model: Some(ModelId::new("host/cheap")),
        }
    );
    let compaction = &outcome.report.model_requests()[0];
    assert_eq!(compaction.purpose(), ModelRequestPurpose::Compaction);
    // Priced at the summary model: 700 input tokens at 3 micro-USD each.
    assert_eq!(compaction.cost().map(|cost| cost.micro_usd), Some(2_100));
    // The unpriced session model leaves the run unpriced.
    assert_eq!(outcome.cost, None);
    assert!(outcome.usage.input >= COMPACTION_INPUT_TOKENS);
}

#[tokio::test]
async fn a_summary_model_leaves_the_registration_sessions_resume_with() {
    // Arrange
    let (smart, smart_requests) = CompactingModel::new(Vec::new());
    let (cheap, _) = CompactingModel::new(Vec::new());
    let catalog = registry_catalog(smart, cheap, Pricing::new(0, 0));
    let store = Arc::new(MemoryStore::new());
    let mut recorded = registered_harness(&catalog, &store)
        .context_policy(ContextPolicy::Evict)
        .session("recorded", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut recorded, 1).await;
    let harness = summarizing_harness(&catalog, &store, "host/cheap");

    // Act
    let mut resumed = harness.resume("recorded").await.expect("session resumes");
    let outcome = resumed.send(input(2)).await;

    // Assert
    assert!(outcome.is_ok());
    assert_eq!(
        resumed.summary_model().map(ModelId::as_str),
        Some("host/cheap")
    );
    assert!(carries(&last_turn_request(&smart_requests), "turn-1"));
}

#[tokio::test]
async fn a_switched_session_compacts_on_the_summary_model_it_sets_again() {
    // Arrange
    let (smart, _) = CompactingModel::new(Vec::new());
    let (cheap, cheap_requests) = CompactingModel::new(Vec::new());
    let catalog = registry_catalog(smart, cheap, Pricing::new(0, 0));
    let store = Arc::new(MemoryStore::new());
    let mut session = summarizing_harness(&catalog, &store, "host/cheap")
        .session("switched", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;

    // Act
    session
        .switch_model(&session_registry(&catalog), "host/next")
        .await
        .expect("model switches");
    let dropped = session.summary_model().cloned();
    let refused = session.set_summary_model(&entry(&catalog, "host/plain"));
    session
        .set_summary_model(&entry(&catalog, "host/cheap"))
        .expect("summary model accepted");
    let (events, _) = streamed_turn(&session, 4).await;

    // Assert
    assert_eq!(dropped, None);
    assert_eq!(
        refused.err().map(|error| error.kind()),
        Some(ErrorKind::Config)
    );
    assert_eq!(
        session.summary_model().map(ModelId::as_str),
        Some("host/cheap")
    );
    assert!(matches!(
        compaction_report(&events).outcome,
        CompactionOutcome::Compacted { model: Some(model), .. } if model.as_str() == "host/cheap"
    ));
    assert_eq!(compactions(&cheap_requests).len(), 1);
}

#[tokio::test]
async fn the_session_model_as_summary_model_compacts_on_itself() {
    // Arrange
    let (smart, smart_requests) = CompactingModel::new(Vec::new());
    let (cheap, cheap_requests) = CompactingModel::new(Vec::new());
    let catalog = registry_catalog(smart, cheap, Pricing::new(0, 0));
    let store = Arc::new(MemoryStore::new());
    let harness = summarizing_harness(&catalog, &store, "host/smart");
    let mut session = harness
        .session("own", object_schema())
        .create()
        .await
        .expect("session created");
    send_turns(&mut session, 3).await;

    // Act
    let (events, _) = streamed_turn(&session, 4).await;

    // Assert
    assert_eq!(harness.summary_model(), None);
    assert!(matches!(
        compaction_report(&events).outcome,
        CompactionOutcome::Compacted { model: Some(model), .. } if model.as_str() == "host/smart"
    ));
    assert_eq!(compactions(&smart_requests).len(), 1);
    assert_eq!(compactions(&cheap_requests).len(), 0);
}

#[test]
fn a_refused_summary_model_keeps_the_previous_one() {
    // Arrange
    let (smart, _) = CompactingModel::new(Vec::new());
    let (cheap, _) = CompactingModel::new(Vec::new());
    let catalog = registry_catalog(smart, cheap, Pricing::new(0, 0));
    let store = Arc::new(MemoryStore::new());
    let mut harness = summarizing_harness(&catalog, &store, "host/cheap");

    // Act
    let error = harness
        .set_summary_model(&entry(&catalog, "host/plain"))
        .expect_err("summary model refused");

    // Assert
    assert_eq!(error.kind(), ErrorKind::Config);
    assert!(error.to_string().contains("host/plain"), "{error}");
    assert_eq!(
        harness.summary_model().map(ModelId::as_str),
        Some("host/cheap")
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
