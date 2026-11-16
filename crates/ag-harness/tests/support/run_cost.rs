//! Run cost in micro-USD and per-turn usage: request costs that sum to the
//! run's cost, catalog list prices and host overrides, unpriced models, and
//! session totals that survive a SQLite resume. A store that cannot record
//! usage fails the turn.

use std::error::Error;
use std::future::poll_fn;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ag_harness::catalog::{Pricing, PricingSource};
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ModelCompletion, ModelMessage, ModelRequest, ModelResponse,
};
use ag_harness::provider::{GPT_6_LUNA, ModelProvider};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::{MemoryStore, SessionStore};
use ag_harness::tool::ToolCall;
use ag_harness::transcript::TurnStatus;
use ag_harness::{
    Cost, ErrorKind, EventStream, Harness, HarnessError, LimitKind, Model, ModelCatalog,
    ModelError, OutputSchema, Role, Run, RunEvent, RunEventKind, RunLimits, RunOutcome, RunRequest,
    TurnRange,
};
use async_trait::async_trait;
use futures_core::Stream;
use serde_json::json;
use tempfile::tempdir;

use crate::context_budget_fixture::unbounded_context_budget;
use crate::model_profile_fixture::unbounded_model_profile;
use crate::store_gate::Gate;

/// Cost of one [`PricedModel`] request under [`host_pricing`]: 800 uncached
/// input tokens at $3, 200 cached at $0.30, and 100 output tokens at $15 per
/// million tokens.
const REQUEST_MICRO_USD: u64 = 2_400 + 60 + 1_500;

/// Answers `{"answer": <prompt>}`, first calling `read` for the prompt
/// `tool`. Every response reports 1,000 input tokens, 200 of them cached,
/// and 100 output tokens, 40 of them reasoning.
struct PricedModel;

#[async_trait]
impl Model for PricedModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let answered = matches!(
            request.messages().last(),
            Some(ModelMessage::ToolResult { .. })
        );
        let response = if request.prompt() == "tool" && !answered {
            ModelResponse::ToolCall(ToolCall::from_json(
                "read-1".into(),
                "read",
                r#"{"path":"a.txt"}"#,
                None,
            )?)
        } else {
            ModelResponse::Output(json!({"answer": request.prompt()}))
        };
        let usage = CompletionUsage::new(Some(200), None, Some(1_000), Some(100), Some(40), None);

        Ok(ModelCompletion::new(
            CompletionMetadata::new("stop".into(), None, None, None, Some(usage)),
            response,
        ))
    }
}

fn answer_schema() -> Result<OutputSchema, Box<dyn Error>> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"]
    }))?)
}

/// Returns negotiated host prices with a cache discount.
fn host_pricing() -> Pricing {
    Pricing::new(3_000_000, 15_000_000).with_cache_read(300_000)
}

/// Returns a harness pricing [`PricedModel`] with [`host_pricing`].
fn priced_harness() -> Result<Harness, HarnessError> {
    Harness::builder()
        .model(Arc::new(PricedModel))
        .context_budget(unbounded_context_budget())
        .pricing(host_pricing())
        .build()
}

/// Returns the micro-USD and source of `cost`.
fn priced(cost: Option<Cost>) -> Option<(u64, PricingSource)> {
    cost.map(|cost| (cost.micro_usd, cost.pricing))
}

/// Runs `run` to its outcome while reading every event of its stream.
async fn streamed(run: Run) -> (Vec<RunEvent>, RunOutcome) {
    let (stream, handle) = run.into_parts();

    tokio::join!(drain(stream), handle.outcome())
}

async fn drain(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await {
        events.push(event);
    }

    events
}

#[tokio::test]
async fn priced_run_streams_request_costs_that_sum_to_its_recorded_cost()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = priced_harness()?.store(Arc::new(MemoryStore::new()));
    let session = harness.session("priced", answer_schema()?).create().await?;

    // Act
    let (events, outcome) = streamed(session.run(RunRequest::new("tool"))).await;
    let transcript = session.transcript(TurnRange::All).await?;
    let totals = session.usage().await?;

    // Assert
    assert!(outcome.result.is_ok());
    let requests: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::ModelRequestCompleted { cost, usage } => Some((priced(*cost), *usage)),
            _ => None,
        })
        .collect();
    let host = PricingSource::Host;
    assert_eq!(
        requests.iter().map(|(cost, _)| *cost).collect::<Vec<_>>(),
        [
            Some((REQUEST_MICRO_USD, host)),
            Some((REQUEST_MICRO_USD, host))
        ]
    );
    assert_eq!(
        requests
            .iter()
            .map(|(_, usage)| usage.reasoning)
            .sum::<u64>(),
        80
    );
    let run_cost = Some((2 * REQUEST_MICRO_USD, host));
    assert_eq!(priced(outcome.cost), run_cost);
    let Some(RunEventKind::Completed(summary)) = events.last().map(|event| &event.kind) else {
        return Err("the last event must be `Completed`".into());
    };
    assert_eq!(priced(summary.cost), run_cost);
    assert_eq!(priced(transcript[0].cost), run_cost);
    assert_eq!(transcript[0].usage, Some(outcome.usage));
    assert_eq!(
        (totals.turns, totals.unknown_turns, totals.unpriced_turns),
        (1, 0, 0)
    );
    assert_eq!(totals.cost_micro_usd, 2 * REQUEST_MICRO_USD);
    assert_eq!(totals.usage, outcome.usage);

    Ok(())
}

#[tokio::test]
async fn catalog_models_bill_at_list_prices_unless_the_host_overrides_them()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let spec = ModelProvider::OpenAi
        .model_spec(GPT_6_LUNA)
        .ok_or("a built-in model")?
        .with_profile(unbounded_model_profile());
    let list_prices = spec.pricing().ok_or("built-in list prices")?;
    let id = spec.id().clone();
    let catalog = Arc::new(
        ModelCatalog::builder()
            .model(spec, Arc::new(PricedModel))
            .assign(Role::SMART, [id])
            .build()?,
    );
    let listed = Harness::builder().catalog(Arc::clone(&catalog)).build()?;
    let negotiated = Harness::builder()
        .catalog(catalog)
        .pricing(Pricing::new(1_000_000, 0))
        .build()?;

    // Act
    let at_list = listed
        .run(RunRequest::new("plain").output(answer_schema()?))
        .await;
    let overridden = negotiated
        .run(RunRequest::new("plain").output(answer_schema()?))
        .await;

    // Assert
    assert_eq!(list_prices.source(), PricingSource::BuiltIn);
    assert_eq!(
        priced(at_list.cost),
        priced(Some(Cost::of(&at_list.usage, list_prices)))
    );
    assert_eq!(
        priced(at_list.cost).map(|(_, source)| source),
        Some(PricingSource::BuiltIn)
    );
    assert_eq!(
        priced(overridden.cost),
        Some((1_000, PricingSource::Host)),
        "cached input falls back to the input price"
    );

    Ok(())
}

#[tokio::test]
async fn unpriced_model_records_usage_without_cost() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness =
        Harness::new(PricedModel, unbounded_context_budget()).store(Arc::new(MemoryStore::new()));
    let session = harness
        .session("unpriced", answer_schema()?)
        .create()
        .await?;

    // Act
    let (events, outcome) = streamed(session.run(RunRequest::new("plain"))).await;
    let transcript = session.transcript(TurnRange::All).await?;
    let totals = session.usage().await?;

    // Assert
    assert_eq!(outcome.cost, None);
    assert!(events.iter().all(|event| match &event.kind {
        RunEventKind::ModelRequestCompleted { cost, .. } => cost.is_none(),
        RunEventKind::Completed(summary) => summary.cost.is_none(),
        _ => true,
    }));
    assert_eq!(transcript[0].cost, None);
    assert_eq!(transcript[0].usage, Some(outcome.usage));
    assert_eq!(
        (totals.turns, totals.unpriced_turns, totals.cost_micro_usd),
        (1, 1, 0)
    );
    assert_eq!(totals.usage.input, 1_000);

    Ok(())
}

#[tokio::test]
async fn sqlite_usage_totals_cover_limit_stopped_turns_and_survive_resume()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempdir()?;
    let database = directory.path().join("sessions.sqlite");
    let session = priced_harness()?
        .database(&database)
        .session("durable", answer_schema()?)
        .create()
        .await?;
    let completed = session.run(RunRequest::new("plain")).await;
    let one_request = RunLimits::default().with_max_model_requests(NonZeroU32::MIN);
    let stopped = session
        .run(RunRequest::new("tool").limits(one_request))
        .await;
    let before = session.usage().await?;
    drop(session);

    // Act
    let resumed = priced_harness()?
        .database(&database)
        .resume("durable")
        .await?;
    let after = resumed.usage().await?;
    let transcript = resumed.transcript(TurnRange::All).await?;

    // Assert
    assert!(completed.result.is_ok());
    assert_eq!(
        stopped.result.map_err(|error| error.kind()).err(),
        Some(ErrorKind::BudgetExhausted(LimitKind::ModelRequests))
    );
    assert_eq!(after, before);
    assert_eq!((after.turns, after.unknown_turns), (2, 0));
    assert_eq!(after.cost_micro_usd, 2 * REQUEST_MICRO_USD);
    assert_eq!(after.usage.input, 2_000);
    assert_eq!(transcript[1].status, TurnStatus::Failed);
    assert_eq!(
        priced(transcript[1].cost),
        Some((REQUEST_MICRO_USD, PricingSource::Host)),
        "a limit-stopped turn keeps the cost of its completed request"
    );

    Ok(())
}

#[tokio::test]
async fn store_that_cannot_record_usage_fails_the_turn() -> Result<(), Box<dyn Error>> {
    // Arrange
    let memory: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let gate = Gate::new(Arc::clone(&memory), false);
    gate.fail_usage.store(true, Ordering::SeqCst);
    let harness = priced_harness()?
        .execution_identity(ExecutionIdentity::new("priced", "1")?)
        .store(Arc::new(gate));
    let mut session = harness
        .session("failing", answer_schema()?)
        .create()
        .await?;

    // Act
    let error = session
        .turn("plain")
        .host_id("request")
        .await
        .err()
        .ok_or("the turn must fail")?;
    let stored = memory.load_transcript("failing", &TurnRange::All).await?;

    // Assert
    assert_eq!(HarnessError::from(error).kind(), ErrorKind::Store);
    assert_eq!(stored[0].status, TurnStatus::Failed);
    assert_eq!(stored[0].usage, None, "nothing was recorded");
    assert_eq!(session.usage().await?.unknown_turns, 1);

    Ok(())
}
