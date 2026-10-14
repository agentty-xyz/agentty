//! Live checks for the features delivered while preparing `ag-harness` for
//! Agentty integration: durable model switching, context budgets, session
//! compaction, sandboxed and unsandboxed Bash, and host-request recovery with
//! image input. Every case runs `AG_LIVE_ROUNDS` times per provider and
//! prints one `LIVE` line per round; a test fails only after every round ran.

use std::io::{self, Write as _};
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ag_harness::bash::{BashConfig, CommandTermination, UnsandboxedExecutor};
use ag_harness::model::{
    ContextBudget, ContextEstimator, HeuristicContextEstimator, ModelCapabilities, ModelClient,
    ModelCompletion, ModelMessage, ModelMetadata, ModelRegistry, ModelRequest, ModelResponse,
};
use ag_harness::provider::{KimiConfig, MUSE_SPARK_1_3, MuseConfig, QWEN_PLUS, QwenConfig};
use ag_harness::recovery::{ExecutionIdentity, HostTurnStatus};
use ag_harness::tool::ToolCall;
use ag_harness::turn::ToolActivity;
use ag_harness::{
    Harness, Model, ModelError, OutputSchema, Repository, Session, SessionError, Tool, ToolPolicy,
    TurnError, TurnOptions,
};
use serde_json::{Value, json};

use crate::context_budget_fixture::unbounded_context_budget;
use crate::{DynError, vision};

const MODEL_API_BASE_URL: &str = "https://api.meta.ai/v1";
const GIT_EXECUTABLE: &str = "/opt/homebrew/bin/git";
const LAUNCHER: &str = env!("CARGO_BIN_EXE_ag-harness-sandbox");

static LAST_KIMI_TURN: Mutex<Option<Instant>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Provider {
    Kimi,
    Muse,
    Qwen,
}

impl Provider {
    const ALL: [Self; 3] = [Self::Kimi, Self::Muse, Self::Qwen];

    fn selected() -> Vec<Self> {
        let filter = std::env::var("AG_LIVE_PROVIDERS").unwrap_or_default();
        let selected: Vec<Self> = Self::ALL
            .into_iter()
            .filter(|provider| filter.is_empty() || filter.split(',').any(|f| f == provider.name()))
            .collect();

        selected
    }

    fn name(self) -> &'static str {
        match self {
            Self::Kimi => "kimi",
            Self::Muse => "muse",
            Self::Qwen => "qwen",
        }
    }

    fn model(self) -> String {
        match self {
            Self::Kimi => std::env::var("KIMI_MODEL").unwrap_or_else(|_| "kimi-k2.6".into()),
            Self::Muse => MUSE_SPARK_1_3.to_string(),
            Self::Qwen => std::env::var("QWEN_MODEL").unwrap_or_else(|_| QWEN_PLUS.into()),
        }
    }

    fn client(self) -> Result<ModelClient, DynError> {
        let model = self.model();
        let client = match self {
            Self::Kimi => ModelClient::kimi(KimiConfig {
                api_key: std::env::var("KIMI_API_KEY")?,
                base_url: std::env::var("KIMI_BASE_URL")?,
                model,
            })?,
            Self::Muse => ModelClient::muse(MuseConfig {
                api_key: std::env::var("MODEL_API_KEY")?,
                base_url: std::env::var("MODEL_API_BASE_URL")
                    .unwrap_or_else(|_| MODEL_API_BASE_URL.to_string()),
                model,
            })?,
            Self::Qwen => ModelClient::qwen(QwenConfig {
                api_key: std::env::var("DASHSCOPE_API_KEY")?,
                base_url: std::env::var("DASHSCOPE_BASE_URL")?,
                model,
            })?,
        };

        Ok(client)
    }

    /// Extra per-turn weight for providers whose replayed reasoning content
    /// counts toward history, so a budget still holds the intended turns.
    fn reasoning_allowance(self) -> u64 {
        match self {
            Self::Kimi => 1000,
            Self::Muse | Self::Qwen => 0,
        }
    }

    /// Kimi's organization limit is a few requests per minute; space its
    /// turns out so retries inside the client do not exhaust their budget.
    async fn pace(self) {
        if self != Self::Kimi {
            return;
        }
        let pace = std::env::var("AG_LIVE_KIMI_PACE_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(40);
        let wait = LAST_KIMI_TURN
            .lock()
            .expect("kimi pacing")
            .and_then(|last| Duration::from_secs(pace).checked_sub(last.elapsed()));
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        *LAST_KIMI_TURN.lock().expect("kimi pacing") = Some(Instant::now());
    }
}

fn rounds() -> usize {
    std::env::var("AG_LIVE_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2)
}

fn capabilities(budget: ContextBudget) -> ModelCapabilities {
    ModelCapabilities {
        context_budget: budget,
        image_input: true,
        tool_calls: true,
    }
}

fn answer_schema() -> Result<OutputSchema, DynError> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    }))?)
}

fn stdout_schema() -> Result<OutputSchema, DynError> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"stdout": {"type": "string"}},
        "required": ["stdout"],
        "additionalProperties": false
    }))?)
}

fn answer(value: &Value) -> String {
    value["answer"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_lowercase()
}

struct Report {
    case: &'static str,
    failures: Vec<String>,
}

impl Report {
    fn new(case: &'static str) -> Self {
        Self {
            case,
            failures: Vec::new(),
        }
    }

    fn record(
        &mut self,
        label: &str,
        round: usize,
        result: Result<String, DynError>,
        started: Instant,
    ) {
        let ms = started.elapsed().as_millis();
        let line = match &result {
            Ok(detail) => format!(
                "LIVE case={} provider={label} round={round} result=pass ms={ms} detail={detail}",
                self.case
            ),
            Err(error) => format!(
                "LIVE case={} provider={label} round={round} result=fail ms={ms} detail={error}",
                self.case
            ),
        };
        let _ = writeln!(io::stdout().lock(), "{line}");
        if result.is_err() {
            self.failures.push(line);
        }
    }

    fn finish(self) -> Result<(), DynError> {
        if self.failures.is_empty() {
            return Ok(());
        }

        Err(format!(
            "{} round(s) failed:\n{}",
            self.failures.len(),
            self.failures.join("\n")
        )
        .into())
    }
}

async fn send(provider: Provider, session: &mut Session, input: &str) -> Result<Value, DynError> {
    provider.pace().await;
    let outcome = session.send(input).await?;

    Ok(outcome.into_output())
}

// ---------------------------------------------------------------------------
// Durable model switching (#664) and registrations (#663)
// ---------------------------------------------------------------------------

async fn switch_roundtrip(from: Provider, to: Provider) -> Result<String, DynError> {
    let mut registry = ModelRegistry::new();
    for provider in [from, to] {
        registry.register(
            ExecutionIdentity::new(provider.name(), "1")?,
            provider.client()?,
            capabilities(unbounded_context_budget()),
        )?;
    }
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("switch.db");
    let harness = Harness::from_registry(&registry, from.name())?.database(&database);
    let mut session = harness.session("switch", answer_schema()?).create().await?;
    let stored = send(
        from,
        &mut session,
        "Remember the code teal-77. Do not repeat it now: set answer to exactly the word stored.",
    )
    .await?;
    let _ = stored;

    let switched = session.switch_model(&registry, to.name()).await;
    match switched {
        Ok(()) => {}
        Err(SessionError::UnsupportedModelHistory { .. }) if from == Provider::Kimi => {
            return Ok(format!(
                "switch {}->{} rejected as documented (Kimi reasoning history is nonportable): {}",
                from.name(),
                to.name(),
                switched.unwrap_err()
            ));
        }
        Err(error) => return Err(format!("switch to {} failed: {error}", to.name()).into()),
    }
    let recalled = send(
        to,
        &mut session,
        "What exact code did I ask you to remember? Set answer to only the code.",
    )
    .await?;
    if !answer(&recalled).contains("teal-77") {
        return Err(format!("{} did not recall after switch: {recalled}", to.name()).into());
    }
    drop(session);

    // The durable selection is now `to`: resuming through the old registration
    // must fail, and the new one must resume with the whole history intact.
    let stale = Harness::from_registry(&registry, from.name())?
        .database(&database)
        .resume("switch")
        .await;
    if !matches!(stale, Err(SessionError::RegistrationMismatch { .. })) {
        return Err(format!("stale registration resumed: {:?}", stale.map(|_| ())).into());
    }
    let mut resumed = Harness::from_registry(&registry, to.name())?
        .database(&database)
        .resume("switch")
        .await?;
    let back = resumed.switch_model(&registry, from.name()).await;
    let back_detail = match back {
        Ok(()) => {
            let again = send(
                from,
                &mut resumed,
                "Set answer to the code I asked you to remember, followed by the word back.",
            )
            .await?;
            if !answer(&again).contains("teal-77") {
                return Err(
                    format!("{} lost history after switching back: {again}", from.name()).into(),
                );
            }
            "switch back ok".to_string()
        }
        Err(SessionError::UnsupportedModelHistory { .. }) if to == Provider::Kimi => {
            "switch back rejected as documented (Kimi reasoning history)".to_string()
        }
        Err(error) => return Err(format!("switch back failed: {error}").into()),
    };

    Ok(format!(
        "{}->{} recall ok; stale-resume rejected; {back_detail}",
        from.name(),
        to.name()
    ))
}

#[tokio::test]
#[ignore = "requires live provider credentials"]
async fn test_model_switch_roundtrip() -> Result<(), DynError> {
    let providers = Provider::selected();
    let mut report = Report::new("model_switch");
    for round in 1..=rounds() {
        for &from in &providers {
            for &to in &providers {
                if from == to {
                    continue;
                }
                let label = format!("{}->{}", from.name(), to.name());
                let started = Instant::now();
                report.record(&label, round, switch_roundtrip(from, to).await, started);
            }
        }
    }

    report.finish()
}

// ---------------------------------------------------------------------------
// Context budget projection (#669)
// ---------------------------------------------------------------------------

fn filler(seed: u8, bytes: usize) -> String {
    let words = [
        "ledger", "harbor", "signal", "meadow", "copper", "lantern", "orbit", "quartz",
    ];
    let mut text = String::new();
    let mut index = usize::from(seed);
    while text.len() < bytes {
        text.push_str(words[index % words.len()]);
        text.push(' ');
        index = index.wrapping_mul(31).wrapping_add(7);
    }

    text
}

fn fact_turn(name: &str, value: &str, seed: u8) -> String {
    format!(
        "Background notes (ignore their content): {}\nRemember fact {name} = {value}. Do not \
         repeat it now: set answer to exactly the word stored.",
        filler(seed, 1200)
    )
}

async fn context_budget_projection(provider: Provider) -> Result<String, DynError> {
    let facts = [
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
        ("kilo", "umber-29"),
        ("lima", "azure-31"),
    ];
    let estimator = HeuristicContextEstimator;
    let turn_weight = |name: &str, value: &str, seed: u8| {
        estimator.message_weight(&ModelMessage::User(fact_turn(name, value, seed)))
            + estimator.message_weight(&ModelMessage::Assistant("{\"answer\":\"stored\"}".into()))
            + 60
            + provider.reasoning_allowance()
    };
    let recall =
        |name: &str| format!("What is the value of fact {name}? Set answer to only the value.");
    let recall_weight = estimator.message_weight(&ModelMessage::User(recall("delta")));
    // Room for roughly the two most recent fact turns; providers whose
    // reasoning is replayed vary per turn, so facts are added until the
    // report shows an eviction instead of trusting the estimate.
    let budget = recall_weight
        + turn_weight("charlie", "indigo-35", 3)
        + turn_weight("delta", "amber-47", 4)
        + 40;
    let mut registry = ModelRegistry::new();
    registry.register(
        ExecutionIdentity::new(provider.name(), "budget-1")?,
        provider.client()?,
        capabilities(ContextBudget::new(NonZeroU64::new(budget).ok_or("budget")?)),
    )?;
    let directory = tempfile::tempdir()?;
    let harness = Harness::from_registry(&registry, provider.name())?
        .database(directory.path().join("budget.db"));
    let mut session = harness.session("budget", answer_schema()?).create().await?;
    let mut latest = facts[0];
    let mut stored_facts = 0;
    for (index, fact) in facts.iter().enumerate() {
        provider.pace().await;
        let stored = session
            .send(fact_turn(fact.0, fact.1, index as u8 + 1))
            .await?;
        latest = *fact;
        stored_facts = index + 1;
        if stored_facts >= 4 && stored.report().history().evicted_turns() >= 1 {
            break;
        }
    }

    provider.pace().await;
    let recent = session.send(recall(latest.0)).await?;
    let recent_history = recent.report().history();
    if recent_history.replayed_turns() == 0 {
        return Err(format!(
            "budget {budget} holds no {} turn at all (evicted {}): raise the reasoning allowance",
            provider.name(),
            recent_history.evicted_turns()
        )
        .into());
    }
    if !answer(recent.output()).contains(latest.1) {
        return Err(format!(
            "recent fact {} not recalled although {} turn(s) were replayed: {}",
            latest.0,
            recent_history.replayed_turns(),
            recent.output()
        )
        .into());
    }
    provider.pace().await;
    let evicted = session
        .send(
            "What is the value of fact alpha? Set answer to only the value, or to the word \
             unknown if this conversation never established it.",
        )
        .await?;
    let evicted_history = evicted.report().history();
    let evicted_answer = answer(evicted.output());
    if evicted_history.evicted_turns() == 0 {
        return Err(format!(
            "budget {budget} never evicted a turn after {stored_facts} facts (replayed {}): lower \
             the reasoning allowance",
            evicted_history.replayed_turns()
        )
        .into());
    }
    if evicted_answer.contains("maroon-11") {
        return Err(format!(
            "evicted fact alpha leaked into the projected request: {evicted_answer}"
        )
        .into());
    }

    // Mandatory content larger than the whole budget fails before any request.
    let oversized = format!(
        "{} Reply stored.",
        filler(9, usize::try_from(budget * 4 + 4096)?)
    );
    let started = Instant::now();
    let rejected = session.send(oversized).await;
    let elapsed = started.elapsed();
    match rejected {
        Err(SessionError::Turn(TurnError::ContextBudgetExceeded {
            budget: reported,
            required,
        })) => {
            if reported != budget || required <= budget || elapsed > Duration::from_secs(1) {
                return Err(format!(
                    "unexpected budget rejection budget={reported} required={required} \
                     elapsed={elapsed:?}"
                )
                .into());
            }
        }
        other => {
            return Err(
                format!("oversized input was not rejected: {:?}", other.map(|_| ())).into(),
            );
        }
    }
    provider.pace().await;
    let after = session.send(recall(latest.0)).await?;
    if !answer(after.output()).contains(latest.1) {
        return Err(format!(
            "session unusable after budget rejection: {}",
            after.output()
        )
        .into());
    }

    Ok(format!(
        "budget={budget} facts={stored_facts} {} recalled (replayed {} evicted {}); alpha evicted \
         (replayed {} evicted {}, model answered `{evicted_answer}`); oversized rejected in \
         {elapsed:?}; session still usable",
        latest.0,
        recent_history.replayed_turns(),
        recent_history.evicted_turns(),
        evicted_history.replayed_turns(),
        evicted_history.evicted_turns()
    ))
}

#[tokio::test]
#[ignore = "requires live provider credentials"]
async fn test_context_budget_projection() -> Result<(), DynError> {
    let mut report = Report::new("context_budget");
    for round in 1..=rounds() {
        for provider in Provider::selected() {
            let started = Instant::now();
            report.record(
                provider.name(),
                round,
                context_budget_projection(provider).await,
                started,
            );
        }
    }

    report.finish()
}

// ---------------------------------------------------------------------------
// Session compaction into checkpoints (#672)
// ---------------------------------------------------------------------------

async fn compaction_checkpoint(provider: Provider) -> Result<String, DynError> {
    let facts = [
        ("alpha", "maroon-11"),
        ("bravo", "olive-23"),
        ("charlie", "indigo-35"),
    ];
    let estimator = HeuristicContextEstimator;
    let turn_weight = |name: &str, value: &str, seed: u8| {
        estimator.message_weight(&ModelMessage::User(fact_turn(name, value, seed)))
            + estimator.message_weight(&ModelMessage::Assistant("{\"answer\":\"stored\"}".into()))
            + 60
    };
    // The session's own system prompt is large, so ordinary requests hold only
    // the two most recent raw turns. Compaction replaces that prompt with its
    // short generation instructions, so its source can still cover every turn.
    let system_prompt = format!(
        "You answer with short JSON. Operating notes (ignore their content): {}",
        filler(5, 2000)
    );
    let system_weight = estimator.message_weight(&ModelMessage::System(system_prompt.clone()));
    let recall = "What is the value of fact alpha? Set answer to only the value, or to the word \
                  unknown if this conversation never established it.";
    let recall_weight = estimator.message_weight(&ModelMessage::User(recall.into()));
    let budget = system_weight
        + recall_weight
        + turn_weight("bravo", "olive-23", 2)
        + turn_weight("charlie", "indigo-35", 3)
        + 40;
    let mut registry = ModelRegistry::new();
    registry.register(
        ExecutionIdentity::new(provider.name(), "compact-1")?,
        provider.client()?,
        capabilities(ContextBudget::new(NonZeroU64::new(budget).ok_or("budget")?)),
    )?;
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("compact.db");
    let harness = Harness::from_registry(&registry, provider.name())?.database(&database);
    let mut session = harness
        .session("compact", answer_schema()?)
        .system_prompt(system_prompt.clone())
        .create()
        .await?;
    for (index, (name, value)) in facts.iter().enumerate() {
        let stored = send(
            provider,
            &mut session,
            &fact_turn(name, value, index as u8 + 1),
        )
        .await?;
        let _ = stored;
    }
    let before = send(provider, &mut session, recall).await?;
    let before_answer = answer(&before);
    if before_answer.contains("maroon-11") {
        return Err(format!("alpha should be evicted before compaction: {before}").into());
    }

    provider.pace().await;
    let checkpoint = session
        .compact()
        .await?
        .ok_or("compact returned None with uncovered turns")?;
    let summary = checkpoint.summary().clone();
    let covered = checkpoint.covered_through();
    if checkpoint.provider().is_none() || checkpoint.model().is_none() {
        return Err(format!("checkpoint lacks model identity: {checkpoint:?}").into());
    }
    if !summary["context"].is_string()
        || !summary["decisions"].is_array()
        || !summary["state"].is_string()
    {
        return Err(format!("summary violates schema: {summary}").into());
    }
    let summary_text = summary.to_string().to_lowercase();
    let mentioned = facts
        .iter()
        .filter(|(_, value)| summary_text.contains(*value))
        .count();

    // A fresh handle must load the checkpoint and answer from it.
    drop(session);
    let mut resumed = Harness::from_registry(&registry, provider.name())?
        .database(&database)
        .resume("compact")
        .await?;
    let loaded = resumed.checkpoint().map(|loaded| loaded.covered_through());
    if loaded != Some(covered) {
        return Err(format!("resumed handle did not load the checkpoint: {loaded:?}").into());
    }
    provider.pace().await;
    let after = resumed.send(recall).await?;
    let after_history = after.report().history();
    if !after_history.checkpoint_replayed() {
        return Err(format!("checkpoint not replayed after resume: {after_history:?}").into());
    }
    let after_answer = answer(after.output());
    let recovered = after_answer.contains("maroon-11");

    // The recall turn is now uncovered: a second compaction advances the
    // boundary, and a third with nothing new returns None.
    provider.pace().await;
    let second = resumed
        .compact()
        .await?
        .ok_or("second compaction returned None")?;
    if second.covered_through() <= covered {
        return Err(format!(
            "second checkpoint did not advance: {} <= {covered}",
            second.covered_through()
        )
        .into());
    }
    let third = resumed.compact().await?;
    if third.is_some() {
        return Err("third compaction ran without uncovered turns".into());
    }
    if !recovered {
        return Err(format!(
            "alpha not recovered from summary (summary mentioned {mentioned}/3 facts; answer \
             `{after_answer}`; summary={summary})"
        )
        .into());
    }

    Ok(format!(
        "budget={budget} evicted before (`{before_answer}`), recovered after compaction; summary \
         mentions {mentioned}/3 facts; covered {covered}->{}",
        second.covered_through()
    ))
}

#[tokio::test]
#[ignore = "requires live provider credentials"]
async fn test_compaction_checkpoint() -> Result<(), DynError> {
    let mut report = Report::new("compaction");
    for round in 1..=rounds() {
        for provider in Provider::selected() {
            let started = Instant::now();
            report.record(
                provider.name(),
                round,
                compaction_checkpoint(provider).await,
                started,
            );
        }
    }

    report.finish()
}

// ---------------------------------------------------------------------------
// In-turn context fitting: elision, reasoning clearing, truncation
// ---------------------------------------------------------------------------

async fn long_tool_loop(provider: Provider) -> Result<String, DynError> {
    let codes = [
        "amber-17",
        "basalt-29",
        "cobalt-31",
        "dune-43",
        "ember-59",
        "fjord-61",
        "granite-73",
        "harbor-89",
    ];
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    let init = tokio::process::Command::new(GIT_EXECUTABLE)
        .args(["init", "--quiet"])
        .current_dir(&root)
        .status()
        .await?;
    if !init.success() {
        return Err("git init failed".into());
    }
    for (index, code) in codes.iter().enumerate() {
        let seed = u8::try_from(index)?;
        let mut lines: Vec<String> = (0..72).map(|line| filler(seed + line, 80)).collect();
        lines.insert(36, format!("The code word of this part is {code}."));
        std::fs::write(
            root.join(format!("part{}.txt", index + 1)),
            lines.join("\n"),
        )?;
    }
    // The large file's code word opens its only line, which alone outweighs
    // the budget. Reads return whole lines, so any result naming the code
    // word must be cut to a head and tail excerpt, whatever offset, limit,
    // or search the model chooses.
    let large = format!(
        "The code word of the large file is ivory-97. {}\n",
        filler(3, 36_000)
    );
    std::fs::write(root.join("large.txt"), large)?;
    let directory = tempfile::tempdir()?;
    let budget =
        ContextBudget::new(NonZeroU64::new(7_000).ok_or("budget")?).with_reserved_output(500)?;
    let harness = Harness::new(provider.client()?, budget)
        .database(directory.path().join("fit.db"))
        .repository(Repository::new(&root, GIT_EXECUTABLE)?)
        .allow(Tool::Read)
        .allow(Tool::Write);
    let mut session = harness.session("fit", answer_schema()?).create().await?;
    provider.pace().await;
    session
        .send(
            "Create notes.txt containing one line `context fitting check` with the write tool, \
             then set answer to `ok`.",
        )
        .await?;
    provider.pace().await;
    let outcome = session
        .send(
            "Read part1.txt through part8.txt in order with the read tool, one file per call and \
             never several in one response, then read large.txt. Each part contains one sentence \
             naming its code word, and large.txt names one at the start of its only line. After \
             reading them all, set answer to the nine code words in that order, separated by \
             commas.",
        )
        .await?;
    let found = answer(outcome.output());
    let missing: Vec<&str> = codes
        .iter()
        .copied()
        .chain(["ivory-97"])
        .filter(|code| !found.contains(code))
        .collect();
    let requests = outcome.report().model_requests();
    let peak = |count: fn(&ag_harness::turn::ContextActivity) -> usize| {
        requests
            .iter()
            .map(|request| count(&request.context()))
            .max()
            .unwrap_or(0)
    };
    let elided = peak(|context| context.elided_results());
    let reasoning = peak(|context| context.cleared_reasoning());
    let writes = peak(|context| context.shrunk_write_calls());
    let dropped = peak(|context| context.dropped_history_turns());
    let truncated = peak(|context| context.truncated_results());
    let rounds = peak(|context| context.dropped_tool_calls());
    let latest_writes = peak(|context| context.shrunk_latest_write_calls());
    let summary = format!(
        "requests={} elided={elided} reasoning={reasoning} writes={writes} dropped={dropped} \
         truncated={truncated} dropped_calls={rounds} latest_writes={latest_writes}",
        requests.len()
    );
    // Eight parts outweigh the budget, so reading them forces elision; the
    // large file's code word only arrives in a truncated result.
    if elided == 0 {
        return Err(format!("older results were never elided: {summary}").into());
    }
    if found.contains("ivory-97") && truncated == 0 {
        return Err(format!("the large result arrived without truncation: {summary}").into());
    }
    if !missing.is_empty() {
        return Err(format!("lost code words {missing:?} ({summary}): {found}").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires live provider credentials"]
async fn test_long_tool_loop_refit() -> Result<(), DynError> {
    let mut report = Report::new("long_tool_loop");
    for round in 1..=rounds() {
        for provider in Provider::selected() {
            let started = Instant::now();
            report.record(
                provider.name(),
                round,
                long_tool_loop(provider).await,
                started,
            );
        }
    }

    report.finish()
}

// ---------------------------------------------------------------------------
// Malformed tool arguments
// ---------------------------------------------------------------------------

/// Answers the first request with one malformed `read` call, as a model that
/// mangled its arguments would, and forwards every later request to the live
/// provider, which must accept the replayed call and its error result.
struct MalformedFirstCall {
    inner: ModelClient,
    injected: AtomicBool,
    raw: &'static str,
}

#[async_trait::async_trait]
impl Model for MalformedFirstCall {
    fn metadata(&self) -> Option<ModelMetadata> {
        Model::metadata(&self.inner)
    }

    fn validate_schema(&self, schema: &OutputSchema) -> Result<(), ModelError> {
        self.inner.validate_schema(schema)
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        if self.injected.swap(true, Ordering::SeqCst) {
            return self.inner.complete(request).await;
        }
        let call = ToolCall::from_json("malformed_read".to_string(), "read", self.raw, None)?;

        Ok(ModelCompletion::from_response(ModelResponse::ToolCall(
            call,
        )))
    }
}

async fn malformed_tool_arguments(
    provider: Provider,
    raw: &'static str,
) -> Result<String, DynError> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    let init = tokio::process::Command::new(GIT_EXECUTABLE)
        .args(["init", "--quiet"])
        .current_dir(&root)
        .status()
        .await?;
    if !init.success() {
        return Err("git init failed".into());
    }
    std::fs::write(root.join("notes.txt"), "The code word is lantern-58.\n")?;
    let directory = tempfile::tempdir()?;
    let model = MalformedFirstCall {
        inner: provider.client()?,
        injected: AtomicBool::new(false),
        raw,
    };
    let harness = Harness::new(model, unbounded_context_budget())
        .database(directory.path().join("malformed.db"))
        .repository(Repository::new(&root, GIT_EXECUTABLE)?)
        .allow(Tool::Read);
    let mut session = harness
        .session("malformed", answer_schema()?)
        .create()
        .await?;
    provider.pace().await;
    let first = session
        .send(
            "Read notes.txt with the read tool and set answer to the code word it names. If a \
             tool call is rejected, fix its arguments and call it again.",
        )
        .await?;
    provider.pace().await;
    // The second turn replays the stored malformed call and its error result.
    let second = session
        .send("Set answer to the code word you found in the previous turn, unchanged.")
        .await?;
    let report = first.report();
    let activities = report
        .tool_calls()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let summary = format!(
        "malformed={} calls={activities:?} first={} second={}",
        report.malformed_tool_calls(),
        answer(first.output()),
        answer(second.output())
    );
    if report.malformed_tool_calls() != 1
        || !matches!(
            report.tool_calls().first(),
            Some(ToolActivity::MalformedArguments { .. })
        )
    {
        return Err(format!("the malformed call was not answered: {summary}").into());
    }
    if !report
        .tool_calls()
        .iter()
        .any(|activity| matches!(activity, ToolActivity::Read { path, .. } if path == "notes.txt"))
    {
        return Err(format!("the model never read notes.txt validly: {summary}").into());
    }
    if !answer(first.output()).contains("lantern-58")
        || !answer(second.output()).contains("lantern-58")
    {
        return Err(format!("the code word was lost: {summary}").into());
    }

    Ok(summary)
}

#[tokio::test]
#[ignore = "requires live provider credentials"]
async fn test_malformed_tool_arguments() -> Result<(), DynError> {
    let mut report = Report::new("malformed_tool_arguments");
    let cases = [
        ("not_json", r#"{"path src/": "notes.txt""#),
        ("unknown_field", r#"{"path src/": "notes.txt"}"#),
    ];
    for round in 1..=rounds() {
        for provider in Provider::selected() {
            for (case, raw) in cases {
                let started = Instant::now();
                report.record(
                    &format!("{}/{case}", provider.name()),
                    round,
                    malformed_tool_arguments(provider, raw).await,
                    started,
                );
            }
        }
    }

    report.finish()
}

// ---------------------------------------------------------------------------
// Context overflow classification
// ---------------------------------------------------------------------------

impl Provider {
    /// Input size whose random words exceed the model's window: Kimi's
    /// 262,144 tokens stay below its two-million tokens-per-minute limit,
    /// which a larger input meets as `429` before the window check, while
    /// Muse and Qwen need over a million tokens.
    fn overflow_input_bytes(self) -> usize {
        match self {
            Self::Kimi => 1024 * 1024,
            Self::Muse | Self::Qwen => 6 * 1024 * 1024,
        }
    }
}

/// Returns `bytes` of pseudo-random lowercase words. Random letters cost
/// more tokens per byte than prose and never repeat, so providers neither
/// compress nor stall on the input.
fn incompressible_text(bytes: usize) -> String {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut text = String::with_capacity(bytes + 16);
    while text.len() < bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        for shift in 0..3 + state % 7 {
            let letter = u8::try_from((state >> (shift * 5)) % 26).unwrap_or(0);
            text.push(char::from(b'a' + letter));
        }
        text.push(' ');
    }

    text
}

async fn context_overflow(provider: Provider) -> Result<String, DynError> {
    let harness = Harness::new(provider.client()?, unbounded_context_budget());
    let input = format!(
        "Set answer to OK.\n{}",
        incompressible_text(provider.overflow_input_bytes())
    );
    provider.pace().await;
    match harness.run_once(input, answer_schema()?).await {
        Err(TurnError::Model(error)) if error.is_context_overflow() => Ok(format!(
            "type={} status={:?}",
            error.error_type().as_str(),
            error.http_status()
        )),
        Err(error) => Err(format!("expected a context overflow, got: {error}").into()),
        Ok(_) => Err("the oversized request was accepted".into()),
    }
}

#[tokio::test]
#[ignore = "requires live provider credentials"]
async fn test_context_overflow_classification() -> Result<(), DynError> {
    let mut report = Report::new("context_overflow_classification");
    for round in 1..=rounds() {
        for provider in Provider::selected() {
            let started = Instant::now();
            report.record(
                provider.name(),
                round,
                context_overflow(provider).await,
                started,
            );
        }
    }

    report.finish()
}

// ---------------------------------------------------------------------------
// Sandboxed Bash (#662), executor contract (#674, #675)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Executor {
    Native,
    Unsandboxed,
}

impl Executor {
    fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Unsandboxed => "unsandboxed",
        }
    }

    fn config(self, timeout: Duration, capture: usize) -> Result<BashConfig, DynError> {
        let config = match self {
            Self::Native => BashConfig::new(
                PathBuf::from(LAUNCHER),
                "/bin/bash".into(),
                "live-native-1".into(),
                timeout,
                capture,
            )?
            .with_read("/bin".into())?
            .with_read("/usr/bin".into())?,
            Self::Unsandboxed => BashConfig::for_executor(
                Arc::new(UnsandboxedExecutor::without_isolation()),
                "/bin/bash".into(),
                "live-unsandboxed-1".into(),
                timeout,
                capture,
            )?,
        };

        Ok(config.with_host_information().with_write("output".into())?)
    }
}

struct BashStep {
    command: &'static str,
    label: &'static str,
    timeout: Duration,
    capture: usize,
}

fn bash_options(executor: Executor, step: &BashStep) -> Result<TurnOptions, DynError> {
    Ok(
        TurnOptions::new(stdout_schema()?, ToolPolicy::default().allow(Tool::Bash))
            .with_bash(executor.config(step.timeout, step.capture)?),
    )
}

fn bash_prompt(command: &str) -> String {
    format!(
        "Use the bash tool exactly once to run this command verbatim, without changing or adding \
         anything:\n{command}\nThen return the command's stdout in the answer, even if it is \
         empty or the command failed."
    )
}

async fn bash_workspace(provider: Provider, executor: Executor) -> Result<String, DynError> {
    let steps = [
        BashStep {
            label: "write-grant",
            command: "tr a-z A-Z < input.txt > output/result.txt; cat output/result.txt",
            timeout: Duration::from_secs(20),
            capture: 4096,
        },
        BashStep {
            label: "denied-write",
            command: "echo leak > escaped.txt 2>/dev/null; echo status=$?",
            timeout: Duration::from_secs(20),
            capture: 4096,
        },
        BashStep {
            label: "network",
            command: "bash -c 'exec 3<>/dev/tcp/1.1.1.1/80' 2>/dev/null; echo net=$?",
            timeout: Duration::from_secs(20),
            capture: 4096,
        },
        BashStep {
            label: "deadline",
            command: "sleep 30; echo finished",
            timeout: Duration::from_secs(3),
            capture: 4096,
        },
        BashStep {
            label: "truncation",
            command: "for i in $(seq 1 400); do echo \"line $i of the sandbox output stream\"; \
                      done",
            timeout: Duration::from_secs(20),
            capture: 2048,
        },
    ];
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    std::fs::create_dir(root.join(".git"))?;
    std::fs::write(root.join(".git/config"), "protected\n")?;
    std::fs::write(root.join("input.txt"), "seed=42\n")?;
    std::fs::create_dir(root.join("output"))?;
    let database = root.join("bash.db");
    let harness = Harness::new(provider.client()?, unbounded_context_budget())
        .repository(Repository::new(&root, GIT_EXECUTABLE)?)
        .execution_identity(ExecutionIdentity::new("live-bash", "1")?)
        .database(&database);
    let mut session = harness.session("bash", stdout_schema()?).create().await?;
    let mut details = Vec::new();
    let mut compliance = Vec::new();

    for (index, step) in steps.iter().enumerate() {
        provider.pace().await;
        let outcome = session
            .turn(bash_prompt(step.command))
            .options(bash_options(executor, step)?)
            .await
            .map_err(|error| format!("{} turn failed: {error}", step.label))?;
        let commands = session.commands().await?;
        let record = commands
            .get(index)
            .ok_or_else(|| format!("{}: no command record", step.label))?;
        let intent = &record.intent;
        let result = record
            .outcome
            .as_ref()
            .ok_or_else(|| format!("{}: command has no outcome", step.label))?;
        if intent.command.trim() != step.command {
            compliance.push(format!(
                "{}: model ran `{}`",
                step.label,
                intent.command.trim()
            ));
        }
        let policy_executor = intent.policy["executor"].as_str().unwrap_or("native");
        if policy_executor != executor.name() {
            return Err(format!("{}: snapshot executor `{policy_executor}`", step.label).into());
        }
        let stdout = result.stdout.trim().to_string();
        let ok = match (step.label, executor) {
            ("write-grant", _) => {
                std::fs::read_to_string(root.join("output/result.txt"))? == "SEED=42\n"
                    && result.termination == CommandTermination::Completed
                    && result.exit_code == Some(0)
                    && stdout.contains("SEED=42")
                    && outcome.output()["stdout"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("SEED=42")
            }
            ("denied-write", Executor::Native) => {
                !root.join("escaped.txt").exists() && stdout.contains("status=1")
            }
            ("denied-write", Executor::Unsandboxed) => {
                // Documented contract: no enforcement, the write lands.
                root.join("escaped.txt").exists() && stdout.contains("status=0")
            }
            ("network", Executor::Native) => stdout.contains("net=1"),
            ("network", Executor::Unsandboxed) => stdout.starts_with("net="),
            ("deadline", _) => {
                result.termination == CommandTermination::Deadline && !stdout.contains("finished")
            }
            ("truncation", _) => result.truncated && result.stdout.len() <= step.capture,
            _ => false,
        };
        if !ok {
            return Err(format!(
                "{}: unexpected outcome termination={:?} exit={:?} truncated={} stdout={stdout:?} \
                 stderr={:?} model_output={}",
                step.label,
                result.termination,
                result.exit_code,
                result.truncated,
                result.stderr.trim(),
                outcome.output()
            )
            .into());
        }
        details.push(format!(
            "{}={:?}/{:?}",
            step.label, result.termination, result.cleanup_scope
        ));
    }

    // Durable recovery: the records survive reopen and none blocks admission.
    drop(session);
    let reopened = Harness::new(provider.client()?, unbounded_context_budget())
        .repository(Repository::new(&root, GIT_EXECUTABLE)?)
        .execution_identity(ExecutionIdentity::new("live-bash", "1")?)
        .database(&database)
        .resume("bash")
        .await?;
    let records = reopened.commands().await?;
    if records.len() != steps.len() || records.iter().any(|record| record.blocks_admission()) {
        return Err(format!(
            "reopened journal has {} records, blocking={}",
            records.len(),
            records.iter().any(|record| record.blocks_admission())
        )
        .into());
    }
    let compliance = if compliance.is_empty() {
        String::new()
    } else {
        format!(
            "; model deviated from verbatim commands: {}",
            compliance.join(" | ")
        )
    };

    Ok(format!(
        "{} steps ok, journal survives reopen: {}{compliance}",
        steps.len(),
        details.join(", ")
    ))
}

#[tokio::test]
#[ignore = "requires live provider credentials"]
async fn test_bash_native_sandbox() -> Result<(), DynError> {
    let mut report = Report::new("bash_native");
    for round in 1..=rounds() {
        for provider in Provider::selected() {
            let started = Instant::now();
            report.record(
                provider.name(),
                round,
                bash_workspace(provider, Executor::Native).await,
                started,
            );
        }
    }

    report.finish()
}

#[tokio::test]
#[ignore = "requires live provider credentials"]
async fn test_bash_unsandboxed_executor() -> Result<(), DynError> {
    let mut report = Report::new("bash_unsandboxed");
    for round in 1..=rounds() {
        for provider in Provider::selected() {
            let started = Instant::now();
            report.record(
                provider.name(),
                round,
                bash_workspace(provider, Executor::Unsandboxed).await,
                started,
            );
        }
    }

    report.finish()
}

// ---------------------------------------------------------------------------
// Host-request recovery with image fingerprints (#667)
// ---------------------------------------------------------------------------

async fn image_host_request(provider: Provider) -> Result<String, DynError> {
    let harness = Harness::new(provider.client()?, unbounded_context_budget())
        .execution_identity(ExecutionIdentity::new("live-image", "1")?)
        .max_history_bytes(NonZeroUsize::new(4 * 1024 * 1024).ok_or("history")?)
        .store(Arc::new(ag_harness::store::MemoryStore::new()));
    let options = TurnOptions::new(vision::color_schema()?, ToolPolicy::default());
    let mut session = harness
        .session("images", vision::color_schema()?)
        .create()
        .await?;
    provider.pace().await;
    let first = session
        .turn(vision::colored_images(vision::REPORT)?)
        .options(options.clone())
        .host_id("req-1")
        .await?;
    vision::assert_colors(first.output(), &format!("{} host request", provider.name()))?;

    let started = Instant::now();
    let retry = session
        .turn(vision::colored_images(vision::REPORT)?)
        .options(options.clone())
        .host_id("req-1")
        .await?;
    let retry_elapsed = started.elapsed();
    if retry != first || retry_elapsed > Duration::from_millis(500) {
        return Err(format!("retry re-executed or differed: elapsed={retry_elapsed:?}").into());
    }
    let recovered = session
        .recover("req-1")
        .await?
        .ok_or("req-1 not recorded")?;
    if !matches!(recovered.status, HostTurnStatus::Completed(ref outcome) if outcome == &first) {
        return Err(format!("recovered status mismatch: {:?}", recovered.status).into());
    }

    // Different image content under the same host ID must conflict.
    let mut swapped = vision::colored_images(vision::REPORT)?;
    swapped = ag_harness::TurnInput::from_blocks(swapped.blocks().iter().rev().cloned().collect())?;
    let conflict = session
        .turn(swapped)
        .options(options.clone())
        .host_id("req-1")
        .await;
    if !matches!(conflict, Err(SessionError::HostTurnConflict)) {
        return Err(format!(
            "changed images did not conflict: {:?}",
            conflict.map(|_| ())
        )
        .into());
    }

    // Text-only follow-up replays the image history.
    provider.pace().await;
    let follow_up = session
        .turn(vision::RECALL)
        .options(options)
        .host_id("req-2")
        .await?;
    vision::assert_colors(
        follow_up.output(),
        &format!("{} replayed after recovery", provider.name()),
    )?;

    Ok(format!(
        "dedup retry in {retry_elapsed:?}; conflict typed; replay ok"
    ))
}

#[tokio::test]
#[ignore = "requires live provider credentials"]
async fn test_image_host_request_recovery() -> Result<(), DynError> {
    let mut report = Report::new("image_recovery");
    for round in 1..=rounds() {
        for provider in Provider::selected() {
            let started = Instant::now();
            report.record(
                provider.name(),
                round,
                image_host_request(provider).await,
                started,
            );
        }
    }

    report.finish()
}
