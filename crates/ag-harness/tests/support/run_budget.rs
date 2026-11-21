//! Shared budgets and concurrency caps: parallel runs drawing on one
//! `SharedBudget` stop at a deterministic cost cutoff and pass the cap only
//! by what requests used beyond their estimates, refused runs settle like
//! limit stops, per-run ledgers override the harness's, sent requests that
//! report no usage are charged their estimate, queued requests hold no
//! budget, and a catalog concurrency cap bounds in-flight requests across
//! harnesses, including ones built from a registration of the entry, and
//! releases its permits when a request is dropped.

use std::error::Error;
use std::future::{IntoFuture, pending, poll_fn};
use std::io;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ag_harness::catalog::{ModelSelector, ModelSpec, Pricing};
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ModelCompletion, ModelMessage, ModelRegistry,
    ModelRequest, ModelResponse,
};
use ag_harness::store::MemoryStore;
use ag_harness::tool::ToolCall;
use ag_harness::{
    BudgetLedger, ErrorKind, EventStream, GenerationParams, Harness, LimitKind, Model,
    ModelCatalog, ModelError, OutputSchema, Role, RunError, RunEvent, RunEventKind, RunLimits,
    RunOutcome, RunRequest, SharedBudget,
};
use async_trait::async_trait;
use futures_core::Stream;
use serde_json::json;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

use crate::model_profile_fixture::unbounded_model_profile;

/// Catalog key of [`BudgetModel`].
const MODEL: &str = "host/budgeted";

/// Output tokens each request reserves, at one micro-USD per output token
/// under [`pricing`].
const OUTPUT_TOKENS: u32 = 100;

/// Estimated cost of one request in micro-USD.
const REQUEST_MICRO_USD: u64 = 100;

/// Output tokens [`BudgetModel`] reports for an `over` request, beyond the
/// reserved [`OUTPUT_TOKENS`].
const OVER_OUTPUT_TOKENS: u64 = 120;

/// Time [`BudgetModel`] takes per answered request.
const REQUEST_TIME: Duration = Duration::from_millis(10);

/// Answers by prompt and counts requests in flight. Answers take
/// [`REQUEST_TIME`] and report 50 input and [`OUTPUT_TOKENS`] output tokens:
/// `answer` with `{"name": "Ada"}`; `two` with a `read` call and, once its
/// result is back, the answer; `over` like `two` but reporting
/// [`OVER_OUTPUT_TOKENS`] output tokens; `unreported` with the answer and no
/// usage. `fail` fails with a provider error; `hang` never answers, after
/// signalling [`Self::hanging`].
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
        let index = self.requests.fetch_add(1, Ordering::SeqCst);
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
        let answered = matches!(
            request.messages().last(),
            Some(ModelMessage::ToolResult { .. })
        );
        let response = match request.prompt() {
            "two" | "over" if !answered => ModelResponse::ToolCall(ToolCall::from_json(
                format!("call-{index}"),
                "read",
                &json!({"path": format!("{index}.txt")}).to_string(),
                None,
            )?),
            "unreported" => {
                return Ok(ModelCompletion::from_response(ModelResponse::Output(
                    json!({"name": "Ada"}),
                )));
            }
            _ => ModelResponse::Output(json!({"name": "Ada"})),
        };
        let output = match request.prompt() {
            "over" => OVER_OUTPUT_TOKENS,
            _ => u64::from(OUTPUT_TOKENS),
        };
        let usage = CompletionUsage::new(None, None, Some(50), Some(output), None, None);

        Ok(ModelCompletion::new(
            CompletionMetadata::new("stop".into(), None, None, None, Some(usage)),
            response,
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
/// cost depends only on its output tokens, which the estimate knows
/// exactly.
fn pricing() -> Pricing {
    Pricing::new(0, 1_000_000)
}

/// Returns a catalog of `model`, priced with [`pricing`], with a maximum
/// output of [`OUTPUT_TOKENS`], and capped at `max_concurrency` requests in
/// flight when set.
fn catalog(
    model: &Arc<BudgetModel>,
    max_concurrency: Option<u32>,
) -> Result<Arc<ModelCatalog>, Box<dyn Error>> {
    let mut spec = ModelSpec::new(MODEL)
        .with_profile(unbounded_model_profile())
        .with_pricing(pricing())
        .with_max_output(u64::from(OUTPUT_TOKENS));
    if let Some(max_concurrency) = max_concurrency {
        spec = spec.with_max_concurrency(NonZeroU32::new(max_concurrency).ok_or("nonzero cap")?);
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

/// Starts `count` stateless runs of `prompt` on `harness` at once and
/// returns their outcomes in start order.
async fn parallel_runs(
    harness: &Harness,
    prompt: &str,
    count: usize,
) -> Result<Vec<RunOutcome>, Box<dyn Error>> {
    let mut tasks = Vec::new();
    for _ in 0..count {
        tasks.push(tokio::spawn(harness.run(request(prompt)?).into_future()));
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

fn budgeted_harness(
    model: &Arc<BudgetModel>,
    budget: &SharedBudget,
    max_concurrency: Option<u32>,
) -> Result<Harness, Box<dyn Error>> {
    Ok(Harness::builder()
        .catalog(catalog(model, max_concurrency)?)
        .budget(Arc::new(budget.clone()))
        .build()?)
}

/// Graph cutoff: eight parallel runs of one request each share a 550
/// micro-USD budget. Every request is estimated and billed at 100
/// micro-USD, so exactly five are admitted, whatever order the runs reach
/// the ledger in; the other three wait for the five to settle and are then
/// refused, since committed spend leaves no room for them.
#[tokio::test(start_paused = true)]
async fn parallel_runs_sharing_one_budget_stop_at_a_deterministic_cost_cutoff()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let budget = SharedBudget::new().with_max_micro_usd(550);
    let harness = budgeted_harness(&model, &budget, None)?;

    // Act
    let outcomes = parallel_runs(&harness, "answer", 8).await?;

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
    assert!(
        model.max_in_flight() > 1,
        "admitted requests overlapped, holding their reservations together"
    );

    Ok(())
}

/// Overshoot bound: eight parallel runs of two requests each share a 1000
/// micro-USD budget, and every request bills 120 micro-USD against its
/// 100 micro-USD estimate. All first requests fit, so runs are cut off
/// between their requests. Committed spend passes the cap by at most the
/// answered requests' 20 micro-USD overshoot each, and every refusal is
/// final: committed spend leaves no room for one more estimate.
#[tokio::test(start_paused = true)]
async fn committed_spend_passes_the_cap_only_by_actual_cost_beyond_estimates()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let cap = 1_000;
    let budget = SharedBudget::new().with_max_micro_usd(cap);
    let harness = budgeted_harness(&model, &budget, None)?;

    // Act
    let outcomes = parallel_runs(&harness, "over", 8).await?;

    // Assert
    let answered = u64::try_from(model.requests())?;
    let overshoot = answered * (OVER_OUTPUT_TOKENS - u64::from(OUTPUT_TOKENS));
    let balance = budget.balance();
    assert!(answered >= 8, "every first request fit");
    assert_eq!(balance.spent_micro_usd, answered * OVER_OUTPUT_TOKENS);
    assert!(balance.spent_micro_usd <= cap + overshoot);
    assert!(balance.spent_micro_usd + REQUEST_MICRO_USD > cap);
    assert_eq!(balance.reserved_micro_usd, 0);
    let cut_off: Vec<_> = outcomes
        .iter()
        .filter(|outcome| outcome.result.is_err())
        .collect();
    assert!(!cut_off.is_empty());
    for outcome in cut_off {
        assert_eq!(
            outcome.result.as_ref().err().map(RunError::kind),
            Some(ErrorKind::BudgetExhausted(LimitKind::Cost))
        );
        assert_eq!(
            outcome.usage.output, OVER_OUTPUT_TOKENS,
            "first request kept"
        );
    }

    Ok(())
}

/// A run whose ledger admits its first request and refuses its second
/// keeps the first request's usage and cost, records them on its session,
/// ends with its `Completed` event, and leaves the session usable. Per-run
/// ledgers replace the harness ledger, which only runs without their own
/// draw from.
#[tokio::test(start_paused = true)]
async fn a_run_refused_mid_run_keeps_its_usage_and_its_session_stays_usable()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let harness_budget = SharedBudget::new();
    let harness = Harness::builder()
        .catalog(catalog(&model, None)?)
        .budget(Arc::new(harness_budget.clone()))
        .store(Arc::new(MemoryStore::new()))
        .build()?;
    let session = harness.session("budget", name_schema()?).create().await?;
    let capped = SharedBudget::new().with_max_micro_usd(150);
    let run_budget = SharedBudget::new();

    // Act
    let (stream, handle) = session
        .run(request("two")?.budget(Arc::new(capped.clone())))
        .into_parts();
    let (events, refused) = tokio::join!(drain(stream), handle.outcome());
    let recorded = session.usage().await?;
    let overridden = session
        .run(request("answer")?.budget(Arc::new(run_budget.clone()) as Arc<dyn BudgetLedger>))
        .await;
    let inherited = session.run(request("answer")?).await;

    // Assert
    assert_eq!(
        refused.result.as_ref().err().map(RunError::kind),
        Some(ErrorKind::BudgetExhausted(LimitKind::Cost))
    );
    assert_eq!(refused.report.model_requests().len(), 1);
    assert_eq!(
        (refused.usage.input, refused.usage.output),
        (50, u64::from(OUTPUT_TOKENS))
    );
    assert_eq!(
        refused.cost.map(|cost| cost.micro_usd),
        Some(REQUEST_MICRO_USD)
    );
    assert!(matches!(
        events.last().map(|event| &event.kind),
        Some(RunEventKind::Completed(summary)) if summary.error.is_some()
    ));
    assert_eq!(recorded.usage.output, u64::from(OUTPUT_TOKENS));
    assert_eq!(
        recorded.cost.map(|cost| cost.micro_usd),
        Some(REQUEST_MICRO_USD)
    );
    assert_eq!(capped.balance().spent_micro_usd, REQUEST_MICRO_USD);
    assert!(overridden.result.is_ok());
    assert!(inherited.result.is_ok());
    assert_eq!(run_budget.balance().spent_micro_usd, REQUEST_MICRO_USD);
    assert_eq!(harness_budget.balance().spent_micro_usd, REQUEST_MICRO_USD);
    assert_eq!(model.requests(), 3);

    Ok(())
}

/// A request answered without usage is charged its whole estimate, whose
/// output comes from the cataloged maximum output when the run sets none.
#[tokio::test(start_paused = true)]
async fn an_answer_without_usage_is_charged_its_estimate() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let budget = SharedBudget::new();
    let harness = budgeted_harness(&model, &budget, None)?;

    // Act
    let outcome = harness
        .run(RunRequest::new("unreported").output(name_schema()?))
        .await;

    // Assert
    assert!(outcome.result.is_ok());
    let balance = budget.balance();
    assert_eq!(balance.spent_micro_usd, REQUEST_MICRO_USD);
    assert!(balance.spent_tokens > u64::from(OUTPUT_TOKENS));
    assert_eq!(balance.reserved_tokens, 0);

    Ok(())
}

/// Sent requests that are cancelled, fail, or are stopped by the
/// wall-clock limit are charged their estimate, since the provider may
/// have billed them, hold no reservation afterwards, and release their
/// concurrency permit for the next request.
#[tokio::test(start_paused = true)]
async fn dropped_and_failed_requests_are_charged_and_release_their_permit()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let budget = SharedBudget::new().with_max_micro_usd(1_000);
    let harness = budgeted_harness(&model, &budget, Some(1))?;
    let hanging = harness.run(request("hang")?).into_handle();
    let cancel = hanging.cancel_token();
    let hanging = tokio::spawn(hanging.outcome());
    model.hanging.notified().await;
    let in_flight = budget.balance();

    // Act
    cancel.cancel();
    let cancelled = hanging.await?;
    let failed = harness.run(request("fail")?).await;
    let timed_out = harness
        .run(
            request("hang")?
                .limits(RunLimits::default().with_max_wall_time(Duration::from_secs(5))),
        )
        .await;
    let answered = timeout(
        Duration::from_secs(60),
        harness.run(request("answer")?).into_future(),
    )
    .await?;

    // Assert
    assert_eq!(in_flight.reserved_micro_usd, REQUEST_MICRO_USD);
    assert_eq!(
        error_kinds(&[cancelled, failed, timed_out, answered]),
        [
            Some(ErrorKind::Cancelled),
            Some(ErrorKind::Overloaded),
            Some(ErrorKind::BudgetExhausted(LimitKind::WallTime)),
            None
        ]
    );
    let balance = budget.balance();
    assert_eq!(
        (balance.reserved_micro_usd, balance.spent_micro_usd),
        (0, 4 * REQUEST_MICRO_USD)
    );
    assert_eq!(model.max_in_flight(), 1);

    Ok(())
}

/// Under a concurrency cap of one, runs queued for the permit hold no
/// budget: only the request in flight is reserved, so a cap that fits the
/// runs one after another admits them all.
#[tokio::test(start_paused = true)]
async fn requests_queued_for_a_concurrency_permit_hold_no_budget() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let budget = SharedBudget::new().with_max_micro_usd(4 * REQUEST_MICRO_USD);
    let harness = budgeted_harness(&model, &budget, Some(1))?;
    let runs = (0..4)
        .map(|_| Ok(tokio::spawn(harness.run(request("answer")?).into_future())))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;

    // Act
    sleep(REQUEST_TIME / 2).await;
    let queued = budget.balance();
    let mut outcomes = Vec::new();
    for run in runs {
        outcomes.push(run.await?);
    }

    // Assert
    assert_eq!(queued.reserved_micro_usd, REQUEST_MICRO_USD);
    assert_eq!(error_kinds(&outcomes), [None; 4]);
    let balance = budget.balance();
    assert_eq!(
        (balance.spent_micro_usd, balance.reserved_micro_usd),
        (4 * REQUEST_MICRO_USD, 0)
    );
    assert_eq!(model.max_in_flight(), 1);

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_catalog_concurrency_cap_bounds_requests_in_flight_across_harnesses()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let model = Arc::new(BudgetModel::default());
    let catalog = catalog(&model, Some(2))?;
    let first = Harness::builder().catalog(Arc::clone(&catalog)).build()?;
    // A registration of the same entry shares its cap with catalog harnesses.
    let mut registry = ModelRegistry::new();
    registry.register_catalog_entry(&catalog.resolve(&ModelSelector::Id(MODEL.into()))?, "1")?;
    let second = Harness::from_registry(&registry, MODEL)?;

    // Act
    let (first, second) = tokio::join!(
        parallel_runs(&first, "answer", 3),
        parallel_runs(&second, "answer", 3)
    );

    // Assert
    let outcomes: Vec<_> = first?.into_iter().chain(second?).collect();
    assert_eq!(error_kinds(&outcomes), [None; 6]);
    assert_eq!(model.requests(), 6);
    assert_eq!(model.max_in_flight(), 2);

    Ok(())
}
