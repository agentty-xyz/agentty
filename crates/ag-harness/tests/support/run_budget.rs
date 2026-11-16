//! Shared budgets and concurrency caps: parallel runs drawing on one
//! `SharedBudget` stop at a deterministic cost cutoff, refused runs settle
//! like limit stops, per-run ledgers override the harness's, cancelled and
//! failed requests release their reservations, and a catalog concurrency
//! cap bounds in-flight requests across harnesses and releases its permits
//! on cancellation.

use std::error::Error;
use std::future::{IntoFuture, pending, poll_fn};
use std::io;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ag_harness::catalog::{ModelSpec, Pricing};
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ModelCompletion, ModelRequest, ModelResponse,
};
use ag_harness::store::MemoryStore;
use ag_harness::{
    BudgetLedger, ErrorKind, EventStream, GenerationParams, Harness, LimitKind, Model,
    ModelCatalog, ModelError, OutputSchema, Role, RunError, RunEvent, RunEventKind, RunOutcome,
    RunRequest, SharedBudget,
};
use async_trait::async_trait;
use futures_core::Stream;
use serde_json::json;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

use crate::model_profile_fixture::unbounded_model_profile;

/// Catalog key of [`BudgetModel`].
const MODEL: &str = "host/budgeted";

/// Output tokens each request reserves and [`BudgetModel`] reports, at one
/// micro-USD per output token under [`pricing`].
const OUTPUT_TOKENS: u32 = 100;

/// Cost of one request in micro-USD, estimated and actual alike.
const REQUEST_MICRO_USD: u64 = 100;

/// Time [`BudgetModel`] takes per answered request.
const REQUEST_TIME: Duration = Duration::from_millis(10);

/// Answers by prompt and counts requests in flight: `answer` with
/// `{"name": "Ada"}` after [`REQUEST_TIME`], reporting 50 input and
/// [`OUTPUT_TOKENS`] output tokens; `fail` with a provider error and no
/// usage; `hang` never, after signalling [`Self::hanging`].
#[derive(Default)]
struct BudgetModel {
    hanging: Notify,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    requests: AtomicUsize,
}

impl BudgetModel {
    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Model for BudgetModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let in_flight = InFlight::enter(&self.in_flight);
        self.max_in_flight
            .fetch_max(in_flight.count, Ordering::SeqCst);
        match request.prompt() {
            "hang" => {
                self.hanging.notify_one();
                pending::<()>().await;
            }
            "fail" => {
                return Err(ModelError::provider(500, None, io::Error::other("down")));
            }
            _ => sleep(REQUEST_TIME).await,
        }
        let usage = CompletionUsage::new(
            None,
            None,
            Some(50),
            Some(u64::from(OUTPUT_TOKENS)),
            None,
            None,
        );

        Ok(ModelCompletion::new(
            CompletionMetadata::new("stop".into(), None, None, None, Some(usage)),
            ModelResponse::Output(json!({"name": "Ada"})),
        ))
    }
}

/// One request counted in flight until dropped, also by cancellation.
struct InFlight<'a> {
    count: usize,
    in_flight: &'a AtomicUsize,
}

impl<'a> InFlight<'a> {
    fn enter(in_flight: &'a AtomicUsize) -> Self {
        let count = in_flight.fetch_add(1, Ordering::SeqCst) + 1;

        Self { count, in_flight }
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Returns free input and one micro-USD per output token, so a request's
/// cost depends only on its output tokens.
fn pricing() -> Pricing {
    Pricing::new(0, 1_000_000)
}

/// Returns a catalog of `model`, priced with [`pricing`] and capped at
/// `max_concurrency` requests in flight when set.
fn catalog(
    model: &Arc<BudgetModel>,
    max_concurrency: Option<u32>,
) -> Result<Arc<ModelCatalog>, Box<dyn Error>> {
    let mut spec = ModelSpec::new(MODEL)
        .with_profile(unbounded_model_profile())
        .with_pricing(pricing());
    if let Some(max_concurrency) = max_concurrency {
        spec = spec.with_max_concurrency(max_concurrency);
    }
    let model: Arc<dyn Model> = Arc::clone(model) as Arc<dyn Model>;

    Ok(Arc::new(
        ModelCatalog::builder()
            .model(spec, model)
            .assign(Role::SMART, [MODEL])
            .build()?,
    ))
}

fn name_schema() -> Result<OutputSchema, Box<dyn Error>> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"]
    }))?)
}

/// Returns a request for `prompt` reserving [`OUTPUT_TOKENS`] of output.
fn request(prompt: &str) -> Result<RunRequest, Box<dyn Error>> {
    let output_tokens = NonZeroU32::new(OUTPUT_TOKENS).ok_or("nonzero output")?;

    Ok(RunRequest::new(prompt)
        .output(name_schema()?)
        .generation(GenerationParams::default().with_max_output_tokens(output_tokens)))
}

/// Starts `count` stateless runs of `answer` on `harness` at once and
/// returns their outcomes in start order.
async fn parallel_runs(harness: &Harness, count: usize) -> Result<Vec<RunOutcome>, Box<dyn Error>> {
    let mut tasks = Vec::new();
    for _ in 0..count {
        tasks.push(tokio::spawn(harness.run(request("answer")?).into_future()));
    }
    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await?);
    }

    Ok(outcomes)
}

fn error_kinds(outcomes: &[RunOutcome]) -> Vec<Option<ErrorKind>> {
    outcomes
        .iter()
        .map(|outcome| outcome.result.as_ref().err().map(RunError::kind))
        .collect()
}

async fn drain(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await {
        events.push(event);
    }

    events
}

/// Graph cutoff: eight parallel runs of one request each share a 550
/// micro-USD budget. Every request is estimated and billed at 100
/// micro-USD, so exactly five are admitted, whatever order the runs reach
/// the ledger in, and the total never passes the cap. The exact bound of
/// [`SharedBudget`] is the cap plus each committed request's actual cost
/// beyond its estimate, here zero.
#[tokio::test(start_paused = true)]
async fn parallel_runs_sharing_one_budget_stop_at_a_deterministic_cost_cutoff()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let budget = Arc::new(SharedBudget::default().with_max_micro_usd(550));
    let ledger: Arc<dyn BudgetLedger> = Arc::clone(&budget) as Arc<dyn BudgetLedger>;
    let harness = Harness::builder()
        .catalog(catalog(&model, None)?)
        .budget(ledger)
        .build()?;

    // Act
    let outcomes = parallel_runs(&harness, 8).await?;

    // Assert
    let kinds = error_kinds(&outcomes);
    let stopped = Some(ErrorKind::BudgetExhausted(LimitKind::Cost));
    assert_eq!(kinds.iter().filter(|kind| kind.is_none()).count(), 5);
    assert_eq!(kinds.iter().filter(|kind| **kind == stopped).count(), 3);
    assert_eq!(model.requests(), 5, "refused runs send no request");
    let balance = budget.balance();
    assert_eq!(
        (
            balance.spent_micro_usd,
            balance.spent_tokens,
            balance.reserved_micro_usd,
            balance.reserved_tokens,
        ),
        (
            5 * REQUEST_MICRO_USD,
            5 * (50 + u64::from(OUTPUT_TOKENS)),
            0,
            0
        )
    );
    assert!(balance.spent_micro_usd <= balance.max_micro_usd.unwrap_or(0));
    assert!(
        model.max_in_flight() > 1,
        "admitted requests overlapped, holding their reservations together"
    );

    Ok(())
}

#[tokio::test]
async fn a_refused_run_settles_like_a_limit_stop_and_its_session_stays_usable()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let harness_budget = Arc::new(SharedBudget::default());
    let ledger: Arc<dyn BudgetLedger> = Arc::clone(&harness_budget) as Arc<dyn BudgetLedger>;
    let harness = Harness::builder()
        .catalog(catalog(&model, None)?)
        .budget(ledger)
        .store(Arc::new(MemoryStore::new()))
        .build()?;
    let session = harness.session("budget", name_schema()?).create().await?;
    let exhausted = Arc::new(SharedBudget::default().with_max_tokens(0));
    let run_budget = Arc::new(SharedBudget::default());

    // Act
    let (stream, handle) = session
        .run(request("answer")?.budget(exhausted))
        .into_parts();
    let (events, refused) = tokio::join!(drain(stream), handle.outcome());
    let overridden = session
        .run(request("answer")?.budget(Arc::clone(&run_budget) as Arc<dyn BudgetLedger>))
        .await;
    let inherited = session.run(request("answer")?).await;

    // Assert
    assert_eq!(
        refused.result.as_ref().err().map(RunError::kind),
        Some(ErrorKind::BudgetExhausted(LimitKind::Tokens))
    );
    assert_eq!(refused.report.model_requests(), []);
    assert!(matches!(
        events.last().map(|event| &event.kind),
        Some(RunEventKind::Completed(summary)) if summary.error.is_some()
    ));
    assert!(overridden.result.is_ok());
    assert!(inherited.result.is_ok());
    assert_eq!(run_budget.balance().spent_micro_usd, REQUEST_MICRO_USD);
    assert_eq!(harness_budget.balance().spent_micro_usd, REQUEST_MICRO_USD);
    assert_eq!(model.requests(), 2);

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn cancelled_and_failed_requests_release_their_reservations() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let budget = Arc::new(SharedBudget::default().with_max_micro_usd(REQUEST_MICRO_USD));
    let ledger: Arc<dyn BudgetLedger> = Arc::clone(&budget) as Arc<dyn BudgetLedger>;
    let harness = Harness::builder()
        .catalog(catalog(&model, None)?)
        .budget(ledger)
        .build()?;
    let hanging = harness.run(request("hang")?).into_handle();
    let cancel = hanging.cancel_token();
    let hanging = tokio::spawn(hanging.outcome());
    model.hanging.notified().await;
    let reserved = budget.balance().reserved_micro_usd;

    // Act
    cancel.cancel();
    let cancelled = hanging.await?;
    let failed = harness.run(request("fail")?).await;
    let answered = harness.run(request("answer")?).await;

    // Assert
    assert_eq!(reserved, REQUEST_MICRO_USD);
    assert_eq!(
        error_kinds(&[cancelled, failed]),
        [Some(ErrorKind::Cancelled), Some(ErrorKind::Overloaded)]
    );
    assert!(
        answered.result.is_ok(),
        "the released reservations leave the whole budget"
    );
    let balance = budget.balance();
    assert_eq!(
        (balance.reserved_micro_usd, balance.spent_micro_usd),
        (0, REQUEST_MICRO_USD)
    );

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_catalog_concurrency_cap_bounds_requests_in_flight_across_harnesses()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let catalog = catalog(&model, Some(2))?;
    let first = Harness::builder().catalog(Arc::clone(&catalog)).build()?;
    let second = Harness::builder().catalog(catalog).build()?;

    // Act
    let (first, second) = tokio::join!(parallel_runs(&first, 3), parallel_runs(&second, 3));

    // Assert
    let outcomes: Vec<_> = first?.into_iter().chain(second?).collect();
    assert_eq!(error_kinds(&outcomes), [None; 6]);
    assert_eq!(model.requests(), 6);
    assert_eq!(model.max_in_flight(), 2);

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_request_releases_its_concurrency_permit() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let harness = Harness::builder()
        .catalog(catalog(&model, Some(1))?)
        .build()?;
    let hanging = harness.run(request("hang")?).into_handle();
    let cancel = hanging.cancel_token();
    let hanging = tokio::spawn(hanging.outcome());
    model.hanging.notified().await;

    // Act
    cancel.cancel();
    let cancelled = hanging.await?;
    let answered = timeout(
        Duration::from_secs(60),
        harness.run(request("answer")?).into_future(),
    )
    .await?;

    // Assert
    assert_eq!(
        error_kinds(&[cancelled, answered]),
        [Some(ErrorKind::Cancelled), None]
    );
    assert_eq!(model.max_in_flight(), 1);

    Ok(())
}
