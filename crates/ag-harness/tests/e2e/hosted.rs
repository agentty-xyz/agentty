//! Live checks for hosted Anthropic and `OpenAI` models: streaming,
//! structured output, reasoning replay, stale reads cleared after a write,
//! prompt caching, history trimming, automatic compaction on a cheaper
//! summary model, usage and cost, and authentication failures. Models come from
//! the environment catalog, so runs are priced. `AG_LIVE_PROVIDERS` selects
//! `anthropic` and `openai`; without it, each runs when its API key is set and
//! is reported as skipped otherwise. Every case prints one `LIVE` line per
//! provider and round.

use std::future::poll_fn;
use std::io::{self, Write as _};
use std::num::{NonZeroU32, NonZeroU64};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ag_harness::catalog::{ModelCatalog, ModelId};
use ag_harness::event::{CompactionOutcome, Delta, Item};
use ag_harness::model::{ContextBudget, ContextPolicy, ReasoningEffort};
use ag_harness::provider::{CLAUDE_SONNET_5_5, GPT_6_1_SOL, ModelConfiguration, ModelProvider};
use ag_harness::store::MemoryStore;
use ag_harness::turn::ModelRequestPurpose;
use ag_harness::{
    Cost, ErrorKind, Harness, HarnessBuilder, OutputMode, OutputSchema, RunError, RunEventKind,
    RunLimits, RunRequest, Session, ToolSet, TurnInput, TurnRange, TurnReport,
};
use futures_core::Stream;
use serde_json::{Value, json};

use crate::DynError;
use crate::context_budget_fixture::unbounded_context_budget;
use crate::features::{
    Provider, Report, answer, answer_schema, fact_turn, filler, notes_repository, rounds,
};

/// Model requests one turn may send, so a misbehaving model cannot loop up
/// a bill.
const MAX_REQUESTS: u32 = 8;
/// Input plus output tokens one turn may use, compaction included.
const MAX_TOKENS: u64 = 200_000;
/// Time that must separate the first and last text deltas of the streamed
/// response. A response buffered and then replayed as deltas emits them
/// within microseconds, while eight streamed sentences take far longer to
/// generate. A share of the request's duration would instead count the
/// silent reasoning a model such as GPT-6 Luna does before its first text.
const MIN_STREAMED_SPREAD: Duration = Duration::from_millis(200);

impl Provider {
    /// Hosted providers to check: those `AG_LIVE_PROVIDERS` names, or else
    /// each whose API key is set. A provider left out, by the filter or for
    /// a missing key, is reported as skipped.
    fn hosted() -> Vec<Self> {
        let filter = std::env::var("AG_LIVE_PROVIDERS").unwrap_or_default();
        let mut selected = Vec::new();
        for provider in [Self::Anthropic, Self::OpenAi] {
            let skipped = if !filter.is_empty() {
                (!filter.split(',').any(|name| name == provider.name()))
                    .then(|| "AG_LIVE_PROVIDERS does not name it".to_string())
            } else if std::env::var(provider.key_env()).is_ok_and(|key| !key.trim().is_empty()) {
                None
            } else {
                Some(format!("{} is not set", provider.key_env()))
            };
            match skipped {
                None => selected.push(provider),
                Some(reason) => {
                    let _ = writeln!(
                        io::stdout().lock(),
                        "LIVE provider={} result=skipped detail={reason}",
                        provider.name()
                    );
                }
            }
        }

        selected
    }

    fn key_env(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Kimi | Self::Muse | Self::Qwen => unreachable!("not a hosted provider"),
        }
    }

    fn catalog_provider(self) -> ModelProvider {
        match self {
            Self::Anthropic => ModelProvider::Anthropic,
            Self::OpenAi => ModelProvider::OpenAi,
            Self::Kimi | Self::Muse | Self::Qwen => unreachable!("not a hosted provider"),
        }
    }

    /// A more capable model whose sessions compact on the provider's cheap
    /// catalog model.
    fn session_model(self) -> String {
        match self {
            Self::Anthropic => CLAUDE_SONNET_5_5.into(),
            Self::OpenAi => GPT_6_1_SOL.into(),
            Self::Kimi | Self::Muse | Self::Qwen => unreachable!("not a hosted provider"),
        }
    }

    fn catalog_id(self, model: &str) -> ModelId {
        ModelId::new(format!("{}/{model}", self.catalog_provider().as_str()))
    }

    /// Returns a builder selecting `model` from the environment catalog, so
    /// runs are priced and summaries go to the provider's cheap model.
    fn harness(self, model: &str) -> Result<HarnessBuilder, DynError> {
        let catalog = ModelCatalog::from_environment()?;

        Ok(Harness::builder()
            .catalog(Arc::new(catalog))
            .model_selector(self.catalog_id(model)))
    }
}

/// Caps every run's model requests and tokens, so a looping model cannot run
/// up a bill.
fn limits() -> Result<RunLimits, DynError> {
    Ok(RunLimits::default()
        .with_max_model_requests(NonZeroU32::new(MAX_REQUESTS).ok_or("limit")?)
        .with_max_total_tokens(NonZeroU64::new(MAX_TOKENS).ok_or("limit")?))
}

/// Runs one session turn under [`limits`] and returns its validated output
/// with its report.
async fn send(
    session: &Session,
    input: impl Into<TurnInput>,
) -> Result<(Value, TurnReport), DynError> {
    let outcome = session.run(RunRequest::new(input).limits(limits()?)).await;
    let output = outcome.result?.parse::<Value>()?;

    Ok((output, outcome.report))
}

/// Runs `case` once per round for every selected hosted provider.
async fn run_case<F, Fut>(name: &'static str, case: F) -> Result<(), DynError>
where
    F: Fn(Provider) -> Fut,
    Fut: Future<Output = Result<String, DynError>>,
{
    let providers = Provider::hosted();
    let mut report = Report::new(name);
    for round in 1..=rounds() {
        for &provider in &providers {
            let started = Instant::now();
            report.record(provider.name(), round, case(provider).await, started);
        }
    }

    report.finish()
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Text deltas must arrive while the response is generated: the first must
/// arrive before the request completes and the last at least
/// [`MIN_STREAMED_SPREAD`] after it, which a response buffered and then
/// replayed as deltas cannot.
async fn streamed_turn(provider: Provider) -> Result<String, DynError> {
    let harness = provider.harness(&provider.model())?.build()?;
    let request = RunRequest::new(
        "Write eight numbered sentences, each about a different lighthouse. Plain text only.",
    )
    .output(OutputMode::Text)
    .limits(limits()?);
    let started = Instant::now();
    let (mut events, handle) = harness.run(request).into_parts();
    let outcome = tokio::spawn(handle.outcome());
    let mut text_deltas = 0_usize;
    let mut first_delta = None;
    let mut last_delta = None;
    let mut completed = None;
    while let Some(event) = poll_fn(|context| Pin::new(&mut events).poll_next(context)).await {
        match event.kind {
            RunEventKind::ItemDelta {
                delta: Delta::Text { .. },
                ..
            } => {
                text_deltas += 1;
                let now = started.elapsed();
                first_delta.get_or_insert(now);
                last_delta = Some(now);
            }
            RunEventKind::ModelRequestCompleted { .. } => {
                completed.get_or_insert_with(|| started.elapsed());
            }
            _ => {}
        }
    }
    let outcome = outcome.await?;
    let text = match outcome.result? {
        ag_harness::RunOutput::Text(text) => text,
        other => return Err(format!("expected text output, got {other:?}").into()),
    };
    let first_delta = first_delta.ok_or("no text delta streamed")?;
    let last_delta = last_delta.ok_or("no text delta streamed")?;
    let completed = completed.ok_or("no model request completed")?;
    let spread = last_delta.saturating_sub(first_delta);
    let summary = format!(
        "deltas={text_deltas} first_delta={first_delta:?} last_delta={last_delta:?} \
         completed={completed:?} chars={}",
        text.len()
    );
    if text_deltas < 2 || first_delta >= completed || spread < MIN_STREAMED_SPREAD {
        return Err(format!("text did not stream while it was generated: {summary}").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires live Anthropic or OpenAI credentials"]
async fn test_streamed_turn() -> Result<(), DynError> {
    // Act & Assert
    run_case("hosted_streamed_turn", streamed_turn).await
}

// ---------------------------------------------------------------------------
// Structured output
// ---------------------------------------------------------------------------

/// A nested schema with an enum, a number, and an array validates natively.
async fn structured_output(provider: Provider) -> Result<String, DynError> {
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {
            "city": {"type": "string"},
            "continent": {
                "type": "string",
                "enum": ["africa", "asia", "europe", "north_america", "south_america", "oceania"]
            },
            "population_millions": {"type": "number"},
            "landmarks": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["city", "continent", "population_millions", "landmarks"],
        "additionalProperties": false
    }))?;
    let harness = provider.harness(&provider.model())?.build()?;
    let outcome = harness
        .run(
            RunRequest::new("Describe Paris, France, naming exactly three landmarks.")
                .output(schema)
                .limits(limits()?),
        )
        .await;
    let output = outcome.result?.parse::<Value>()?;
    let landmarks = output["landmarks"].as_array().map_or(0, Vec::len);
    if output["continent"] != "europe"
        || !output["population_millions"].is_number()
        || !(2..=4).contains(&landmarks)
    {
        return Err(format!("unexpected structured output: {output}").into());
    }

    Ok(format!(
        "corrections={} output={output}",
        outcome.report.output_corrections()
    ))
}

#[tokio::test]
#[ignore = "requires live Anthropic or OpenAI credentials"]
async fn test_structured_output() -> Result<(), DynError> {
    // Act & Assert
    run_case("hosted_structured_output", structured_output).await
}

// ---------------------------------------------------------------------------
// Reasoning replay
// ---------------------------------------------------------------------------

/// A tool loop with reasoning enabled records the provider's reasoning
/// (signed thinking for Claude, encrypted reasoning for `OpenAI`), and later
/// turns replay it verbatim; a provider that rejects the replayed blocks
/// fails the follow-up turns.
async fn reasoning_replay(provider: Provider) -> Result<String, DynError> {
    let (_workspace, repository) = notes_repository().await?;
    let harness = provider
        .harness(&provider.model())?
        .store(Arc::new(MemoryStore::new()))
        .repository(repository)
        .tools(ToolSet::empty().with_read())
        .model_reasoning_effort(ReasoningEffort::Medium)
        .build()?;
    let session = harness
        .session("reasoning", answer_schema()?)
        .create()
        .await?;
    let (first, first_report) = send(
        &session,
        "Read notes.txt with the read tool. Its code word ends in a two-digit number: work out \
         that number multiplied by 7, and set answer to the code word followed by a space and the \
         product.",
    )
    .await?;
    let (second, _) = send(
        &session,
        "Set answer to the product you computed in the previous turn, digits only.",
    )
    .await?;
    let (third, _) = send(
        &session,
        "Set answer to the code word from the first turn, unchanged.",
    )
    .await?;
    let transcript = session.transcript(TurnRange::All).await?;
    // Claude's thinking stays readable in the transcript. Encrypted `OpenAI`
    // reasoning without a summary shows no transcript item, so for it the
    // reported reasoning tokens show only that the model reasoned; the
    // follow-up turns below then check that its replay was accepted.
    let reasoning = transcript.first().map_or(0, |turn| {
        turn.items
            .iter()
            .filter(|item| matches!(item, Item::Reasoning { .. }))
            .count()
    });
    let reasoning_tokens = transcript
        .first()
        .and_then(|turn| turn.usage)
        .map_or(0, |usage| usage.reasoning);
    let summary = format!(
        "turn1_requests={} turn1_reasoning_items={reasoning} \
         turn1_reasoning_tokens={reasoning_tokens} answers=[{}|{}|{}]",
        first_report.model_requests().len(),
        answer(&first),
        answer(&second),
        answer(&third)
    );
    let reasoned = reasoning > 0 || (provider == Provider::OpenAi && reasoning_tokens > 0);
    if !reasoned {
        return Err(format!("the first turn recorded no reasoning: {summary}").into());
    }
    if !answer(&first).contains("406")
        || !answer(&second).contains("406")
        || !answer(&third).contains("lantern-58")
    {
        return Err(format!("replayed turns lost the first turn's work: {summary}").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires live Anthropic or OpenAI credentials"]
async fn test_reasoning_replay() -> Result<(), DynError> {
    // Act & Assert
    run_case("hosted_reasoning_replay", reasoning_replay).await
}

// ---------------------------------------------------------------------------
// Stale reads
// ---------------------------------------------------------------------------

/// A large file read and then rewritten leaves a stale read that the next
/// request clears within the budget. The check is that the clearing happens
/// and the provider accepts the following requests with that result
/// replaced by its notice; the final answer only confirms that the turn
/// finished normally.
async fn stale_read_cleared(provider: Provider) -> Result<String, DynError> {
    let (workspace, repository) = notes_repository()?;
    let notes = (0..60)
        .map(|line| format!("{}\n", filler(line, 120)))
        .chain(["The code word is lantern-58.\n".to_string()])
        .collect::<String>();
    std::fs::write(workspace.path().join("notes.txt"), notes)?;
    let harness = provider
        .harness(&provider.model())?
        .store(Arc::new(MemoryStore::new()))
        .repository(repository)
        .tools(ToolSet::empty().with_read().with_write())
        .model_reasoning_effort(ReasoningEffort::Low)
        .build()?;
    let session = harness.session("stale", answer_schema()?).create().await?;
    let (output, report) = send(
        &session,
        "Read all of notes.txt with the read tool. Then use the write tool to change only its \
         last line so the code word is lantern-99. Then read notes.txt again from line 55 to the \
         end and set answer to the code word its last line names now.",
    )
    .await?;
    let superseded = report
        .model_requests()
        .iter()
        .map(|request| request.context().superseded_results())
        .max()
        .unwrap_or_default();
    let summary = format!(
        "requests={} superseded={superseded} answer={}",
        report.model_requests().len(),
        answer(&output)
    );
    if superseded == 0 {
        return Err(format!("no stale read was cleared: {summary}").into());
    }
    if !answer(&output).contains("lantern-99") {
        return Err(format!("the rewritten code word was not reported: {summary}").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires live Anthropic or OpenAI credentials"]
async fn test_stale_read_cleared() -> Result<(), DynError> {
    // Act & Assert
    run_case("hosted_stale_read_cleared", stale_read_cleared).await
}

// ---------------------------------------------------------------------------
// Prompt cache
// ---------------------------------------------------------------------------

/// A repeated session prefix well above the providers' minimum cacheable
/// length is served from the prompt cache on the next turn.
async fn prompt_cache(provider: Provider) -> Result<String, DynError> {
    let harness = provider
        .harness(&provider.model())?
        .store(Arc::new(MemoryStore::new()))
        .build()?;
    let session = harness
        .session("cache", answer_schema()?)
        .system_prompt(format!(
            "You answer with short JSON. Reference notes (ignore their content): {}",
            filler(11, 16_000)
        ))
        .create()
        .await?;
    let first = session
        .run(RunRequest::new("Set answer to the word first.").limits(limits()?))
        .await;
    first.result?;
    let second = session
        .run(RunRequest::new("Set answer to the word second.").limits(limits()?))
        .await;
    second.result?;
    let summary = format!(
        "first(input={} cache_write={:?} cache_read={}) second(input={} cache_read={})",
        first.usage.input,
        first.usage.cache_write,
        first.usage.cache_read,
        second.usage.input,
        second.usage.cache_read
    );
    if second.usage.cache_read == 0 {
        return Err(format!("the repeated prefix was not read from the cache: {summary}").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires live Anthropic or OpenAI credentials"]
async fn test_prompt_cache_hit() -> Result<(), DynError> {
    // Act & Assert
    run_case("hosted_prompt_cache", prompt_cache).await
}

// ---------------------------------------------------------------------------
// History trimming with replayed reasoning
// ---------------------------------------------------------------------------

/// Facts stored by a reasoning session under a budget that holds only a few
/// turns; Claude binds each signed thinking block to the history before it.
const TRIM_FACTS: [(&str, &str); 10] = [
    ("alpha", "maroon-11"),
    ("bravo", "olive-23"),
    ("charlie", "indigo-35"),
    ("delta", "amber-47"),
    ("echo", "cobalt-59"),
    ("foxtrot", "sienna-61"),
    ("golf", "violet-73"),
    ("hotel", "ochre-85"),
    ("india", "teal-97"),
    ("juliet", "coral-13"),
];

/// A small budget evicts the oldest turns of a reasoning session, which
/// changes the history ahead of replayed thinking. Two more fact turns and
/// a recall follow the first eviction, and each must succeed: Claude models
/// that bind thinking to its history must not reject the trimmed request
/// (the harness drops the invalidated blocks).
async fn history_trimming(provider: Provider) -> Result<String, DynError> {
    let budget =
        ContextBudget::new(NonZeroU64::new(4_000).ok_or("budget")?).with_reserved_output(1_000)?;
    let harness = provider
        .harness(&provider.model())?
        .store(Arc::new(MemoryStore::new()))
        .context_budget(budget)
        .model_reasoning_effort(ReasoningEffort::Low)
        .build()?;
    let session = harness.session("trim", answer_schema()?).create().await?;
    let mut evicted = Vec::new();
    let mut latest = TRIM_FACTS[0];
    let mut after_eviction = None;
    for (index, fact) in TRIM_FACTS.iter().enumerate() {
        let (_, report) = send(
            &session,
            fact_turn(fact.0, fact.1, u8::try_from(index)? + 1),
        )
        .await
        .map_err(|error| format!("turn {} failed: {error} (evicted {evicted:?})", index + 1))?;
        evicted.push(report.history().evicted_turns());
        latest = *fact;
        if after_eviction.is_none() && evicted[index] > 0 {
            after_eviction = Some(0);
        } else if let Some(turns) = after_eviction.as_mut() {
            *turns += 1;
            if *turns == 2 {
                break;
            }
        }
    }
    let (name, value) = latest;
    let (recall, recall_report) = send(
        &session,
        format!("What is the value of fact {name}? Set answer to only the value."),
    )
    .await
    .map_err(|error| format!("recall failed: {error} (evicted {evicted:?})"))?;
    let summary = format!(
        "evicted_per_turn={evicted:?} recall_evicted={} answer={}",
        recall_report.history().evicted_turns(),
        answer(&recall)
    );
    if after_eviction != Some(2) {
        return Err(format!("too few turns followed the first eviction: {summary}").into());
    }
    if !answer(&recall).contains(value) {
        return Err(format!("the latest fact was lost: {summary}").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires live Anthropic or OpenAI credentials"]
async fn test_history_trimming_with_reasoning() -> Result<(), DynError> {
    // Act & Assert
    run_case("hosted_history_trimming", history_trimming).await
}

// ---------------------------------------------------------------------------
// Automatic compaction on the summary model
// ---------------------------------------------------------------------------

/// A session on the provider's more capable model compacts automatically,
/// and the catalog sends the compaction to its cheap model, which must
/// carry the oldest fact into the checkpoint.
///
/// On Claude models this fails until the Anthropic request translation
/// drops the JSON Schema keywords the Messages API rejects in
/// `output_config.format`: the compaction summary schema's `maxItems` is
/// answered with HTTP 400, which the reported compaction outcomes show.
async fn automatic_compaction(provider: Provider) -> Result<String, DynError> {
    let facts = &TRIM_FACTS[..8];
    let session_model = provider.session_model();
    let budget =
        ContextBudget::new(NonZeroU64::new(4_000).ok_or("budget")?).with_reserved_output(1_000)?;
    let harness = provider
        .harness(&session_model)?
        .store(Arc::new(MemoryStore::new()))
        .context_budget(budget)
        .context_policy(ContextPolicy::auto_compact_at(50).ok_or("threshold")?)
        .model_reasoning_effort(ReasoningEffort::Low)
        .build()?;
    let summary_model = harness
        .summary_model()
        .cloned()
        .ok_or("the catalog resolved no summary model")?;
    let session = harness
        .session("compact", answer_schema()?)
        .create()
        .await?;
    let mut compactions = Vec::new();
    let mut outcomes = Vec::new();
    let mut checkpoint = None;
    let mut cost = 0_u64;
    let mut turns = 0;
    for (index, (name, value)) in facts.iter().enumerate() {
        let (mut events, handle) = session
            .run(
                RunRequest::new(fact_turn(name, value, u8::try_from(index)? + 1)).limits(limits()?),
            )
            .into_parts();
        let outcome = tokio::spawn(handle.outcome());
        while let Some(event) = poll_fn(|context| Pin::new(&mut events).poll_next(context)).await {
            if let RunEventKind::Compacted(report) = event.kind {
                if let CompactionOutcome::Compacted {
                    covered_through, ..
                } = &report.outcome
                {
                    checkpoint = Some(covered_through.get());
                }
                outcomes.push(format!("turn{}:{:?}", index + 1, report.outcome));
            }
        }
        let outcome = outcome.await?;
        outcome.result?;
        turns += 1;
        for request in outcome.report.model_requests() {
            cost += request.cost().map_or(0, |cost| cost.micro_usd);
            if request.purpose() == ModelRequestPurpose::Compaction {
                compactions.push(request.model().map(ToString::to_string));
            }
        }
        if checkpoint.is_some() {
            break;
        }
    }
    let checkpoint = checkpoint.ok_or_else(|| {
        format!(
            "no checkpoint after {turns} turns (compaction requests {compactions:?}, outcomes \
             {outcomes:?})"
        )
    })?;
    let (recall, recall_report) = send(
        &session,
        "What is the value of fact alpha? Set answer to only the value, or to the word unknown if \
         this conversation never established it.",
    )
    .await?;
    let summary = format!(
        "session={session_model} summary_model={summary_model} turns={turns} \
         compactions={compactions:?} outcomes={outcomes:?} checkpoint_through={checkpoint} \
         replayed={} cost_micro_usd={cost} answer={}",
        recall_report.history().checkpoint_replayed(),
        answer(&recall)
    );
    if compactions.is_empty()
        || compactions
            .iter()
            .any(|model| model.as_deref() != Some(summary_model.as_str()))
    {
        return Err(format!("compaction did not run on the summary model: {summary}").into());
    }
    if !answer(&recall).contains("maroon-11") {
        return Err(format!("the checkpoint lost the oldest fact: {summary}").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires live Anthropic or OpenAI credentials"]
async fn test_automatic_compaction_on_summary_model() -> Result<(), DynError> {
    // Act & Assert
    run_case("hosted_automatic_compaction", automatic_compaction).await
}

// ---------------------------------------------------------------------------
// Usage and cost
// ---------------------------------------------------------------------------

/// Reported usage is complete and the run's cost is the catalog price of
/// that usage, summed over its requests.
async fn usage_and_cost(provider: Provider) -> Result<String, DynError> {
    let model = provider.model();
    let pricing = provider
        .catalog_provider()
        .pricing(&model)
        .ok_or_else(|| format!("{model} has no catalog price"))?;
    let harness = provider.harness(&model)?.build()?;
    let outcome = harness
        .run(
            RunRequest::new("Set answer to the capital of Japan.")
                .output(answer_schema()?)
                .limits(limits()?),
        )
        .await;
    let output = outcome.result?.parse::<Value>()?;
    let cost = outcome.cost.ok_or("the priced run reported no cost")?;
    let requests = outcome.report.model_requests();
    let summed: u64 = requests
        .iter()
        .map(|request| request.cost().map_or(0, |cost| cost.micro_usd))
        .sum();
    let usage = outcome.usage;
    let summary = format!(
        "requests={} input={} output={} cache_read={} cache_write={:?} reasoning={} \
         cost_micro_usd={} answer={}",
        requests.len(),
        usage.input,
        usage.output,
        usage.cache_read,
        usage.cache_write,
        usage.reasoning,
        cost.micro_usd,
        answer(&output)
    );
    if usage.input == 0 || usage.output == 0 || usage.unreported_requests != 0 {
        return Err(format!("usage is incomplete: {summary}").into());
    }
    if summed != cost.micro_usd {
        return Err(format!("request costs sum to {summed}: {summary}").into());
    }
    if requests.len() == 1 && Cost::of(&usage, pricing).micro_usd != cost.micro_usd {
        return Err(format!("the cost is not the catalog price of the usage: {summary}").into());
    }
    // One short request costs well under a cent on the cheap models.
    if cost.micro_usd == 0 || cost.micro_usd > 10_000 {
        return Err(format!("implausible cost: {summary}").into());
    }
    if !answer(&output).contains("tokyo") {
        return Err(format!("wrong answer: {summary}").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires live Anthropic or OpenAI credentials"]
async fn test_usage_and_cost() -> Result<(), DynError> {
    // Act & Assert
    run_case("hosted_usage_and_cost", usage_and_cost).await
}

// ---------------------------------------------------------------------------
// Authentication failure
// ---------------------------------------------------------------------------

/// A rejected API key fails at once as a non-retryable `Auth` error.
async fn invalid_key(provider: Provider) -> Result<String, DynError> {
    let model = ModelConfiguration::new(provider.catalog_provider(), provider.model())
        .client_from_environment(|name| {
            if name == provider.key_env() {
                Ok("invalid-live-check-key".to_string())
            } else {
                std::env::var(name)
            }
        })?;
    let harness = Harness::new(model, unbounded_context_budget());
    let started = Instant::now();
    let result = harness
        .run_once("Set answer to OK.", answer_schema()?)
        .await;
    let elapsed = started.elapsed();
    let error = match result {
        Ok(outcome) => {
            return Err(format!("an invalid key was accepted: {}", outcome.output()).into());
        }
        Err(error) => RunError::from(error),
    };
    let summary = format!(
        "kind={:?} retryable={} elapsed={elapsed:?}",
        error.kind(),
        error.is_retryable()
    );
    if error.kind() != ErrorKind::Auth || error.is_retryable() || elapsed > Duration::from_secs(20)
    {
        return Err(format!("unexpected failure for an invalid key: {summary} ({error})").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires network access to Anthropic or OpenAI"]
async fn test_invalid_key_is_not_retried() -> Result<(), DynError> {
    // Act & Assert
    run_case("hosted_invalid_key", invalid_key).await
}
