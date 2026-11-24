# `ag-harness`

Structured LLM turns with deny-by-default tools and durable sessions.

- **Every answer is typed.** Each turn ends in JSON validated locally against your
  schema, or, in `OutputMode::Text`, in plain text.
- **Tools are off until you add them.** A turn offers exactly the tools of its
  `ToolSet`: the built-in read, write, and sandboxed Bash, and your own `ToolHandler`s.
- **Your store owns the conversation.** Sessions resume from SQLite (or your own store)
  in any process; the provider never holds the only copy.

## Quickstart

```rust
use std::env;
use std::sync::Arc;

use ag_harness::provider::{GPT_6_LUNA, ModelConfiguration, ModelProvider};
use ag_harness::{Harness, OutputSchema};
use serde_json::json;

async fn summarize() -> Result<(), Box<dyn std::error::Error>> {
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": { "summary": { "type": "string" } },
        "required": ["summary"],
    }))?;
    let model = ModelConfiguration::new(ModelProvider::OpenAi, GPT_6_LUNA)
        .client_from_environment(|name| env::var(name))?;
    let harness = Harness::builder().model(Arc::new(model)).build()?;

    let outcome = harness.run_once("Summarize Cargo.toml", schema).await?;
    println!("{}", outcome.output()["summary"]);

    Ok(())
}
```

Every harness needs a `ContextBudget`: the approximate request weight the model's
context window allows, in roughly token-sized units. The builder derives it from the
model's declared `ModelProfile`: built-in `OpenAI`, `ChatGPT`, and Qwen flash models
declare one. For other models, set it with `context_budget`. `build` checks only the
model, the budget, and that a default set holding built-ins has a repository, and
reports a failure as a `HarnessError`; storage opens on first session use. `run_once`
keeps no history. For a conversation, use a session.

A `Harness` is a cheap, `Clone + Send + Sync` handle: clones share one configuration and
one SQLite connection pool, so one harness can serve many concurrent sessions and turns.
Separate harnesses or processes may share a SQLite file, including a new one: opens
create a missing file already in WAL mode and apply pending schema migrations under the
write lock. Each harness waits up to `store::SQLITE_BUSY_TIMEOUT` for another
connection's lock, and as long for a pooled connection, before reporting an error;
connections a host opens to the same file should use it as their busy timeout.
`Harness::new` and `Harness::from_registry` remain available.

## Embedding

Add the crate with `cargo add ag-harness`. [`examples/embed.rs`](examples/embed.rs)
shows a complete host: one model through `ModelClient::from_spec`, one host tool behind
an approval handler, and the run's event stream. Run it against any OpenAI-compatible
server with
`AG_HARNESS_EXAMPLE_ENDPOINT=http://127.0.0.1:8080/v1 cargo run -p ag-harness --example embed`.

The native Bash sandbox runs each command through a helper executable of the same
`ag-harness` version. Provide it in one of two ways:

- **Your own executable.** Call `ag_harness::sandbox::dispatch()` first in `main`,
  before creating a runtime or threads, and configure Bash with `BashConfig::native`.
  When the harness starts your executable as the helper, `dispatch` runs it and returns
  its exit code, which `main` returns at once; otherwise it returns `None`.

  ```rust,no_run
  fn main() -> std::process::ExitCode {
      if let Some(helper) = ag_harness::sandbox::dispatch() {
          return helper;
      }
      // The host program starts here.
      std::process::ExitCode::SUCCESS
  }
  ```

- **The standalone helper.** Install the version matching your `ag-harness` dependency
  with
  `cargo install ag-harness --version <version> --features native-sandbox --bin ag-harness-sandbox`,
  outside every workspace the harness runs commands in, and pass its absolute path to
  `BashConfig::new`. A helper of another version fails its commands.

On Linux the helper runs inside the sandbox, so it needs read access to the shared
libraries it links (`ldd` lists them) as well as Bash's: `with_system_reads` grants the
system directories, and `with_read` anything else. A host executable usually links more
than the standalone helper. Without them the sandbox starts but each command fails
closed. On Linux the sandbox also needs the system Bubblewrap (`apt install bubblewrap`
or `dnf install bubblewrap`) and unprivileged user namespaces; macOS ships Seatbelt.
Where the sandbox cannot run, commands run unsandboxed with a warning unless you choose
otherwise; see [Bash](#bash).

## Sessions

```rust
use std::path::Path;
use std::sync::Arc;

use ag_harness::model::ContextBudget;
use ag_harness::{Harness, Model, OutputSchema, Repository, ToolSet};

async fn review(
    model: Arc<dyn Model>,
    budget: ContextBudget,
    git_executable: &Path,
    schema: OutputSchema,
) -> Result<(), Box<dyn std::error::Error>> {
    let harness = Harness::builder()
        .model(model)
        .context_budget(budget)
        .sqlite("harness.db")
        .repository(Repository::new(".", git_executable)?)
        .tools(ToolSet::empty().with_read())
        .build()?;

    let mut session = harness
        .session("review-42", schema)
        .system_prompt("Keep the review concise.")
        .create()
        .await?;
    let outcome = session.send("Review the current changes").await?;

    // Later, in the same or another process:
    let mut session = harness.resume("review-42").await?;
    let outcome = session.send("Now focus on error handling").await?;

    Ok(())
}
```

Every request replays finished turns within the context budget and the history byte
limit; the provider never holds conversation state. Unless `max_history_bytes` sets it,
a new session's byte limit is about what the model's declared context window leaves for
history, at most 256 KiB, when the context budget comes from that window; otherwise it
is 256 KiB. Sessions keep their limit on resume and model switches. An older turn may
replay with its tool results, reasoning, and `write` patches replaced by short notices,
or else as its input and final answer, and a turn larger than eight times the byte limit
loads only as its input and final answer. Those forms are sticky: each turn repeats the
history the previous turn sent and appends that turn, so provider prompt caches keep the
prefix, and only history that outgrows a limit is shortened, newest first, to half of
it. Each model response and tool result is stored as soon as it finishes. A failed or
interrupted turn replays what it recorded plus a note naming why it stopped and any
writes or commands whose outcome is unknown; when it does not fit, it replays with its
tool traffic replaced by notices, or else as only its input and note. Sessions run
concurrently, with one active turn per session. The library never picks a database
location for you.

### Transcripts and forks

```rust
use ag_harness::event::{SessionId, TurnIndex};
use ag_harness::{Harness, Session, TurnRange};

async fn retry(harness: &Harness, session: &Session) -> Result<(), Box<dyn std::error::Error>> {
    let turns = session.transcript(TurnRange::Last(5)).await?;
    let retry = harness
        .fork(&SessionId::new("review-42"), TurnIndex::new(1), SessionId::new("review-42b"))
        .await?;

    Ok(())
}
```

`Session::transcript` returns settled turns as serializable `TurnRecord`s: status, the
`ErrorKind` a failed or cancelled turn stopped with, input, per-run instructions, the
same `event::Item`s live runs emit (tool calls report `answered` or `unanswered`), the
final output, timestamps, and usage and cost. `TurnRange` selects all turns, the newest
N, or an index span, and only those turns are read; history the replay budget leaves out
is still included. `Session::replayed_range` returns the span of turns the handle
replays ahead of its next input. `Harness::fork` copies a session's settled turns
through one index, with their write, command, and host request records, into a new
session that keeps the source's output mode, system prompt, and model. The fork's own ID
gives it its own prompt-cache key, and the source is unchanged. If the fork cannot be
resumed with the harness's model, it stays stored; resume it with a matching harness.

`Session::usage` sums the tokens and cost of every settled turn into `UsageTotals`. Each
turn records its usage when admitted and as its model requests complete, so failed,
limit-stopped, and interrupted turns keep the usage of their completed requests; a
failed usage write stops the turn. Turns stored before usage was recorded take unpriced
usage from their stored outcome when run with a host request ID, and otherwise count as
`unknown_turns`, never as zero.

## Custom tools

Every tool is a `ToolHandler`: it returns its `ToolDefinition`, declares its
`ToolEffects`, and answers each call's JSON arguments with a `ToolOutput`. A `ToolSet`
lists handlers in the order the model sees them; names are unique, and `read`, `write`,
and `bash` are reserved for the built-ins.

Implementing `ToolHandler` needs the `async-trait` crate, as implementing `Model` does.

```rust
use std::sync::Arc;

use ag_harness::tool::{
    ToolContext, ToolDefinition, ToolDefinitionError, ToolEffects, ToolError, ToolOutput,
};
use ag_harness::{Harness, Model, ToolHandler, ToolSet};
use serde_json::{Value, json};

struct Clock(ToolDefinition);

impl Clock {
    fn new() -> Result<Self, ToolDefinitionError> {
        ToolDefinition::new("clock", "Read the clock.", json!({"type": "object"})).map(Self)
    }
}

#[async_trait::async_trait]
impl ToolHandler for Clock {
    fn definition(&self) -> ToolDefinition {
        self.0.clone()
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::ReadOnly
    }

    async fn call(&self, _args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(r#"{"time":"12:00"}"#))
    }
}

fn clock_harness(model: Arc<dyn Model>) -> Result<Harness, Box<dyn std::error::Error>> {
    let tools = ToolSet::builder().handler(Arc::new(Clock::new()?)).build()?;

    Ok(Harness::builder().model(model).tools(tools).build()?)
}
```

- `HarnessBuilder::tools` sets the default set, `RunRequest::tools` replaces it for one
  run, and `TurnOptions::with_tools` for one turn; sets never merge, so extend a default
  with `ToolSet::to_builder`. Without a set, a turn offers no tools.
- A turn offers exactly the tools of its set. Select built-ins with
  `ToolSet::builtins()`, `ToolSet::with_read`, `with_write`, and `with_bash`, or the
  builder's `read`, `write`, and `bash`; they need a repository, host handlers do not.
  `with_*` is idempotent and keeps built-ins in their advertised order whatever the call
  order, while the builder rejects a name added twice. The set reads each definition
  once, and only a call naming the tool exactly runs its handler.
- Harness defaults carry no `BashConfig`, so a default set holding `bash`, such as
  `ToolSet::builtins()`, needs `RunRequest::bash` or `TurnOptions::with_bash` on every
  run or turn; for plain defaults, use `ToolSet::empty().with_read().with_write()`.
- `ToolError::rejected` sends feedback to the model and the turn continues, as do
  arguments that are not a JSON object, which also count toward the malformed-response
  limit. A result over 64 KiB is dropped and the model is told the call ran.
  `ToolError::failed` or a panic fails the turn with `ErrorKind::ToolFailed`. Reports
  list host calls as `ToolActivity::Host` with a `HostToolOutcome`.
- Identical host calls with identical results count toward repetition detection, like
  built-in calls: the model is nudged, then the turn stops.
- `ToolContext` names the call, its run, its session (only for a host-created session),
  and the workspace; `ToolContext::cancelled` resolves when the turn is cancelled, which
  also drops the call's future.

### Approvals

`HarnessBuilder::approval` gates every call to a tool whose `ToolEffects` are not
`ReadOnly`, such as the built-in `write` and `bash`, behind your `ApprovalHandler`;
read-only calls never ask. Without a handler, every tool in the set runs unasked.

```rust
use std::sync::Arc;
use std::time::Duration;

use ag_harness::{ApprovalHandler, ApprovalRequest, Decision, Harness, Model, ToolSet};

struct AskTheUser;

#[async_trait::async_trait]
impl ApprovalHandler for AskTheUser {
    async fn decide(&self, request: &ApprovalRequest) -> Decision {
        match request.tool.as_str() {
            "write" => Decision::Allow,
            _ => Decision::Deny { feedback: "not now".into() },
        }
    }
}

// A `bash` call of a program `BashConfig::with_excluded_command` excludes is
// marked `ToolEffects::ExecutesUnsandboxed` and carries its exact argument
// vector in `request.command`. Only `Decision::AllowUnsandboxed` runs it
// outside the sandbox; `Allow` and `AllowWith` keep it inside.

fn approved_harness(
    model: Arc<dyn Model>,
    tools: ToolSet,
) -> Result<Harness, Box<dyn std::error::Error>> {
    Ok(Harness::builder()
        .model(model)
        .tools(tools)
        .approval(Arc::new(AskTheUser))
        .approval_timeout(Duration::from_secs(60))
        .build()?)
}
```

- A run emits `RunEventKind::ApprovalRequested` for the call, then asks the handler.
- `Allow` runs the call; `AllowWith(arguments)` runs it with replacement arguments that
  must match the tool's parameters schema. The session keeps the model's call, and the
  result gains an `approved_arguments` field so the model knows what ran.
  `Deny { feedback }` answers the model with the feedback and the run continues.
- A run emits `ApprovalRequested` only when its events are streamed; read-only calls are
  never gated, so declare a host tool `Writes` or `Executes` to have it approved.
- A handler that does not decide within `approval_timeout` (five minutes by default)
  fails the run with `ErrorKind::ApprovalTimeout`; the call never runs and the session
  is released for its next turn. Cancelling the run drops the pending decision.

## Turns

`run_once` and `send` run a turn with the defaults. When a turn needs more, call `turn`
and chain what you need before awaiting it:

```rust
use ag_harness::{OutputSchema, Session, ToolSet, TurnOptions};

async fn review(
    session: &mut Session,
    schema: OutputSchema,
) -> Result<(), Box<dyn std::error::Error>> {
    let outcome = session
        .turn("Review against main")
        .options(TurnOptions::from_output(schema).with_tools(ToolSet::empty().with_read()))
        .host_id("request-7")
        .await?;

    Ok(())
}
```

| Need                                    | Chain                                       |
| --------------------------------------- | ------------------------------------------- |
| Different schema or tools               | `.options(TurnOptions::from_output(...))`   |
| Safe retries under a host request ID    | `.host_id(id)`, later `session.recover(id)` |
| Cancellation and settlement observation | `.start()` instead of `.await`              |
| A one-shot turn with explicit options   | `harness.turn(input, options)`              |

- **Options replace defaults for that turn; they never merge.** Session defaults stay
  unchanged.
- **Host IDs make a turn idempotent.** A retry with the same ID and the same effective
  request returns the recorded outcome without calling the model or tools again. This
  requires an `ExecutionIdentity` on the harness (or a registered model).
- **`start()` returns a `ControlledTurn`.** Its `control()` can `cancel()` the turn and
  then wait on `settled()`, independently of the caller's future, until persistence,
  writes, and command cleanup have finished. A failure names its `SettlementPhase`;
  `retry_settlement()` retries persistence and command cleanup.
- **Runs settle before they report.** `session.run(RunRequest)` and
  `harness.run(RunRequest)` return a `Run`; awaiting it yields a `RunOutcome` only after
  persistence, writes, and commands settle (cleanup is retried once). The outcome
  carries `result` (`RunOutput` with `parse::<T>()`, or a `RunError`), `usage` summed
  over every model request the run completed, even when it failed or was cancelled, its
  `cost`, the run's `files_changed` and `commands`, and its `report`. Cleanup that still
  fails is reported in `settlement`, whose `Unsettled::retry()` readmits the session; it
  never replaces `result`. A cloneable `CancelToken` from `run.cancel_token()`, or
  `into_handle()`, cancels it from any task. `harness.run` keeps its single turn in a
  fresh in-memory session and requires `RunRequest::output`.
- **Run options apply to one run.** `RunRequest::output` selects an `OutputMode` (a JSON
  schema, or `Text` for a `RunOutput::Text` answer) over the session's stored default;
  `Harness::session` accepts either as that default. `RunRequest::instructions` adds
  host instructions after the system prompt and replayed history, so the cached prefix
  survives changing them; they are stored with the turn but not replayed to later runs.
  `RunRequest::generation` sends `GenerationParams`, such as `max_output_tokens`, to the
  provider. Text output needs a model whose `Model::supports_text_output` holds
  (`ModelClient` does), and a bound must suit the provider (the ChatGPT plan takes none,
  the OpenAI API at least 16); otherwise the run fails with `ErrorKind::Config` before
  any request. Providers that state the schema in a leading prompt message (Kimi, Qwen,
  OpenAI-compatible) miss the cached prefix when a run changes the output mode.
- **Runs can be bounded.** `RunRequest::limits` takes `RunLimits` for model requests,
  tool calls, input plus output tokens, and wall-clock time; every limit is unset by
  default and none is stored with the session. A run that reaches one fails with
  `ErrorKind::BudgetExhausted(LimitKind)`, which is not retryable, keeps the usage of
  its completed requests, and leaves the session ready for the next run; on the wire,
  `kind` stays `"budget_exhausted"` with the limit in `limit`. Requests and tokens are
  checked before each request, so the last request, or one context summary, may
  overshoot. Tokens count cached input in full, and requests without reported usage
  count as zero. Wall time starts once the session admits the run and also stops a
  pending request or tool call; a stopped tool call leaves its turn interrupted.
- **Runs can share a budget.** A `BudgetLedger` reserves a `budget::Estimate` before
  every model request, summaries and re-sends included, and commits the actual usage and
  cost after it. Once sent, a request that reports no usage, fails, or is dropped in
  flight is charged its whole estimate, since the provider may have billed it. Set one
  ledger for a harness with `HarnessBuilder::budget` (default `budget::Unlimited`) or
  for one run with `RunRequest::budget`, which replaces the harness ledger for that run.
  The built-in `SharedBudget` caps total micro-USD and tokens across every run given it.
  A request blocked only by other open reservations waits for them to settle; it is
  refused only when committed spend and its estimate pass a cap, so a refusal is final.
  The total exceeds a cap only by what requests used beyond their estimates. The
  estimate reserves `GenerationParams::max_output_tokens`, else the cataloged
  `ModelSpec::max_output`, else the model's reserved output; set one so the reserve
  covers real responses. A refused request ends the run with
  `ErrorKind::BudgetExhausted(LimitKind::Cost)` or `(LimitKind::Tokens)`, settled like a
  `RunLimits` stop, and reports `budget_ledger_cost` or `budget_ledger_tokens` in
  telemetry.
- **Runs report cost in micro-USD.** A `Cost` is integer micro-USD with its
  `catalog::PricingSource`, from the `Pricing` of the model's catalog entry: built-in
  list prices, `CatalogBuilder::pricing`, or `ModelSpec::with_pricing` for a host model.
  `Usage` counts overlap: `cache_read` and a known `cache_write` are part of `input`,
  and `reasoning` is part of `output`. Uncached input, cache reads, cache writes, and
  output bill at their own prices, and cache reads and writes fall back to the input
  price when no cache price is declared; built-in prices declare each provider's
  published cache-hit price, and a cache-write price where the provider charges one.
  Each request's exact cost rounds to the nearest micro-USD, halves up, and sums
  saturate at `u64::MAX`. Models without prices, such as models set with
  `HarnessBuilder::model` or `switch_model`, OpenAI-compatible servers, and plan-billed
  ChatGPT models, report no cost.
- **Runs stream typed events.** `run.into_parts()` returns an `EventStream` (a
  `futures::Stream` of `RunEvent`) and a `RunHandle`. `EventStream::run_id` returns the
  run ID before the run starts. Each event has the run ID, a gap-free `seq`, and a
  timestamp in Unix milliseconds. Events report the admitted turn with its session, turn
  index, and catalog model ID, assistant text, reasoning, tool calls with their
  arguments (`ToolCallRequested`, before each call runs) and results with structured
  outcomes such as a command's exit code, per-request usage and cost, context
  reductions, each retry a built-in client sends (`Retrying`, with its attempt, delay,
  and `ErrorKind`, before the wait), and a successful run's committed `Output` item. A
  built-in client on any protocol (Responses, Chat Completions, or Anthropic Messages)
  streams each turn response as `ItemDelta` events (`Delta::Text`, `Delta::Reasoning`,
  or `Delta::ToolArguments`). Each goes to the response's reasoning, message, or
  tool-call item, which starts at its first fragment and completes after the response. A
  streamed final answer's text completes a `Message` item before the validated `Output`.
  A discarded response, such as an invalid answer that is re-requested, completes its
  reasoning and message items with the text it streamed; its tool-call items stay open.
  `Completed` is always the last event, also after a failure or cancellation; items
  still open then were interrupted. It follows settlement, so the transcript then holds
  the terminal status of a run that emitted `Started`, unless persistence cleanup
  failed. Delivery is lossless: the run waits while the stream is full, so read it while
  awaiting the outcome. Cancelling still resolves a run whose stream is full. Dropping
  the stream discards events without stalling the run. Events carry session content, so
  never export them as telemetry. Every type derives `serde` and `schemars::JsonSchema`;
  the schema is committed in `crates/ag-harness/schema/run_event.json`.
- **Turns have no tool-call limit.** Cancellation and the `ContextBudget` bound a turn.
  Within a turn, requests only append, so they share one cached prefix, until tool
  traffic grows a request past the budget, or until clearing the older results that
  later calls superseded would save a quarter of a request; that request replaces all of
  them with notices naming the newer call. An over-budget request is sent as a reduced
  copy without an extra model call, reduced deeply once so several later requests append
  to it unchanged. First, older tool results that a later call superseded (a re-read of
  the same target, an applied write to the file read, or a rerun of the same command)
  become notices naming that call. Then the new round's results above 25% of the input
  budget are cut (a `read` result by whole lines or items, any other to a head and tail
  excerpt), and its largest results are cut to one common size while together they
  exceed 50%. Then, down to 50% of the budget and oldest first, the other older tool
  results become notices (`read` notices name the target and say its content is not
  available until read again; `bash`/`write` notices keep the outcome and say not to
  repeat), then older reasoning and `write` patches are cleared (the text a model wrote
  beside its calls, `ToolCall::assistant_text`, is kept); the rounds holding the newest
  five results are reduced only after everything older, and the newest rounds'
  reasoning, within 10% of the input budget, is kept after their results are cleared, so
  a model's notes in reasoning outlive the results they describe (it is cleared before a
  replayed turn would be dropped, and those rounds are dropped below only when the
  request would not fit otherwise). If that is not enough, the oldest replayed turns and
  then the current turn's oldest tool rounds are dropped whole, down to 70%, behind an
  appended note listing the files they modified and read, the commands they ran, and
  their other reads, whose results are not available until read again. Only as a last
  resort are the latest tool round's results cut further and its `write` patches
  replaced by their recorded outcome. Every reduced message keeps that form in later
  requests. Stored messages are never changed; `ModelRequestActivity::context` reports
  each reduction. A request that still cannot fit fails with
  `TurnError::ContextBudgetExceeded`.
- **Earlier rounds can be summarized instead.** `Harness::context_summaries` with
  `ContextSummaryPolicy::Enabled` lets a request that still exceeds 50% of the budget
  after superseded results are cleared and oversized results capped first ask the model,
  in one extra request, to summarize the current turn's earlier tool rounds. The latest
  round and the rounds before it within a quarter of the budget stay whole; the rest are
  sent as one note carrying the summary and listing their files, commands, and reads, in
  this and later requests only. A summary is requested only when the rounds not yet
  offered to one weigh a fifth of the budget, and is accepted when it matches the
  summary schema, fits 15% of the budget, and is smaller than its source. When
  `Model::keeps_prefix_across_schemas` holds (Kimi and Qwen through `ModelClient`), the
  request is the turn's last request with the summary instruction appended and
  `ModelRequest::prefix_schema` set, so the provider serves it from the cached prefix;
  otherwise, or when that does not fit, it is a tool-free request of its own at low
  reasoning effort. A cataloged summary model (see [Model catalog](#model-catalog))
  always gets a request of its own. Invalid output is corrected like a final output; a
  failed summary falls back to the steps above, and the turn stops asking after two
  failures. Summary requests count toward the turn's model requests
  (`ModelRequestPurpose::ContextSummary`); `ContextActivity::summarized_tool_calls`,
  `TurnReport::context_summary_failures`, and `ContextSummary*` lifecycle events report
  them. Summaries are disabled by default.
- **Requests are weighed with the model's estimator.** `HeuristicContextEstimator`
  weighs message payload and tool definitions at per-model rates of tokens per 1,000
  bytes, plus fixed tokens per message, per tool definition, and per request, and can
  count an output schema the provider renders into the prompt. `DEFAULT` is four bytes
  per token with four tokens per item; a `ModelProfile` declares measured parameters
  with `with_estimator`, and harnesses built from its registration use them.
- **Provider usage calibrates the estimate.** Each response's reported input tokens
  calibrate later requests: a line through the first reported request, sloped by the
  steepest token growth since and lifted over every report, so providers' fixed
  per-request cost does not inflate large requests. A first request below a tenth of the
  input budget only offsets the estimate by its excess tokens until a larger request
  measures the slope. Projection, admission, and fitting keep calibrated requests within
  95% of the input budget, and use 90% until usage measured the slope. The calibration
  lives in memory, is shared by a harness and its sessions, and resets when a session
  switches model or the estimator is replaced. `ModelRequestActivity::estimated_weight`
  and `calibrated_weight` report what each request was fitted as.
- **A provider context-window rejection is recovered in the turn.** A request the
  provider rejects with `ModelErrorType::ContextOverflow` caps later requests at 75% of
  its estimated weight, and the turn refits and re-sends it, up to twice per turn. A
  refit that cannot shrink below the cap, or a third rejection, fails the turn with the
  provider's error. The cap is part of the calibration, so later turns stay below it
  until the model or estimator changes. `TurnReport::context_overflow_recoveries` counts
  the re-sends, and `LifecycleEventKind::ContextOverflowRetried` reports each one.
- **The estimate margin follows measured error.**
  `ModelProfile::with_estimate_margin(max_output, under_count_percent)` reserves the
  maximum output plus the margin that keeps a request filling 95% of the input budget
  within the window while its real tokens run `under_count_percent` over the estimate.
  Declare the estimator's largest measured under-count plus a buffer; denser content is
  left to calibration and overflow recovery.
- **Requests carry a prompt-cache key.** Every turn request of a durable session carries
  one key derived from a hash of the session ID, stable across handles and resume; each
  stateless turn gets a fresh key shared by its tool loop. `ModelRequest::cache_key`
  exposes it to `Model` adapters. Built-in providers send it as `prompt_cache_key` to
  Muse, Kimi, and `OpenAI`, which route requests sharing a key to the backend holding
  their cached prefix, and leave it off for Qwen. The `ChatGPT` plan also receives it as
  the `session-id` header, which that backend routes by. Checkpoint and round-summary
  requests carry none.
- **Read results are bounded as they run.** One `read` result may weigh 25% of the
  calibrated input budget: a `file` or `show` read keeps its leading whole lines and
  states the lines shown, the file's line count, and the exact `offset` and `limit` that
  fetch the next page (a first line too long to show whole shows its first bytes). A
  `list` or `search` keeps whole items, a `diff` whole lines (or the first bytes of the
  first one when none fits). One response's results may weigh 50% together: the first
  that fit are kept, and every later `read` is answered with a `deferred` result
  repeating its exact arguments, reported as `ToolActivity::ReadDeferred`, for the model
  to fetch in a later response.
- **Identical reads are answered from the earlier result.** A `read` whose arguments
  match an earlier read of the same turn returns a short note naming that call instead
  of running again, while the last request still carried the earlier result whole, no
  `write` or `bash` call ran since, and the result is larger than the note. The turn
  report lists it as `ToolActivity::ReadRepeated`. `write` and `bash` always run.
- **Invalid final output gets two corrections.** A final answer that is not valid JSON
  or violates the schema is re-requested with short, request-only feedback up to twice
  per turn; `TurnReport::output_corrections` counts these retries, each rejected request
  is reported with `ModelRequestActivity::rejected_output`, and a third rejection fails
  the turn.
- **Malformed tool calls get error results.** A call whose arguments its tool cannot
  decode, such as invalid JSON or a misspelled field, is journaled with its raw
  arguments and answered with an error result naming the problem and the tool's fields,
  while the other calls of the same response still run.
  `TurnReport::malformed_tool_calls` counts them and each is listed as
  `ToolActivity::MalformedArguments`. A third consecutive response made only of
  malformed calls to enabled tools fails the turn; any other call resets the count.
- **Unavailable tools get error results.** A call to a tool the turn does not enable, or
  to a tool that does not exist, does not run and does not fail the turn: it is
  journaled and answered with a `"status": "unavailable"` error result listing the
  available tools, emits `LifecycleEventKind::ToolDenied` with a
  `ToolUnavailableReason`, and is listed as `ToolActivity::Unavailable`. A name
  providers could not replay, such as `container.exec`, is kept with each other
  character replaced by `_`; only an empty name fails the response. Tool metrics record
  these calls with `error.type` `tool_unavailable`.
- **Repeated tool calls are nudged, then stopped.** A call that matches an earlier one
  in tool, arguments, and result is counted within the turn's last 12 calls, including
  reads answered from the earlier result; every call to an unavailable tool counts as
  identical, whatever its name and arguments. A `write` or `bash` call that ran unlike
  any in that window restarts the count. At three identical calls the next request
  carries request-only feedback asking the model to stop, counted by
  `TurnReport::repetition_nudges`; at five the turn fails with
  `TurnError::RepeatedToolCalls`. Both emit `LifecycleEventKind::ToolCallsRepeated`.

Input is anything `Into<TurnInput>`: a string, or ordered `InputBlock`s mixing text with
PNG/JPEG images for models that declare image support.

## Tools and permissions

| Tool    | Add with              | Does                               | Also requires                                 |
| ------- | --------------------- | ---------------------------------- | --------------------------------------------- |
| `read`  | `ToolSet::with_read`  | Read, list, search, and show files | A `Repository`                                |
| `write` | `ToolSet::with_write` | Apply one bounded unified diff     | A `Repository`                                |
| `bash`  | `ToolSet::with_bash`  | Run a command in a sandbox         | `TurnOptions::with_bash(BashConfig)` per turn |

`Repository::new` requires a trusted Git executable outside the worktree. Comparisons
(`diff`, `show` of the base side) additionally need a host-pinned `ComparisonBase` in
`TurnOptions::with_comparison_base`.

Tool arguments tolerate common model quirks without widening the advertised schema:
`read` `offset` and `limit` also accept a canonical decimal string such as `"500"`.
`write` recounts hunk line counts from the hunk body, like `git apply --recount`. A hunk
applies at its header line when it matches there exactly or ignoring trailing
whitespace; otherwise it applies only at its single exact match in the file as already
changed by earlier hunks, or failing that its single match ignoring trailing whitespace.
The result's `adjusted_hunks` reports each relocated or whitespace-tolerant hunk.
Ambiguous or unmatched hunks reject the whole patch, and insertion-only hunks still need
an exact `-N,0` anchor. A `read` `list` or `search` path that matches no repository file
returns a corrective tool error rather than an empty result.

### Bash

`BashConfig::native` and `BashConfig::new` select the native sandbox: Bubblewrap,
Landlock, and seccomp on Linux; Seatbelt on macOS. Both need a helper executable, set up
as [Embedding](#embedding) describes, and a trusted Bash. `BashConfig::for_executor`
selects your own `BashExecutor` instead, such as
`UnsandboxedExecutor::without_isolation` for hosts already inside a container or VM.
Each executor reports its `Enforcement` (none, filesystem, or filesystem and network),
and the `bash` tool description tells the model exactly that.

`HarnessBuilder::sandbox`, or `Harness::sandbox` on a harness built another way such as
`Harness::from_registry`, decides what a native configuration does where the sandbox
cannot run: no helper, no `bwrap` (default `/usr/bin/bwrap`), unprivileged user
namespaces disabled, no usable `/usr/bin/sandbox-exec`, or another platform. Before a
turn offering `bash` is admitted, the harness checks the sandbox until one check
succeeds for the configuration, so a remedy applied while the host runs takes effect at
the next turn:

| `Sandbox`                                          | Where the sandbox cannot run                                               |
| -------------------------------------------------- | -------------------------------------------------------------------------- |
| `Native { if_unavailable: Unsandboxed }` (default) | Commands run without isolation, and every such turn reports a warning      |
| `Native { if_unavailable: Refuse }`                | Each turn offering `bash` fails with `ErrorKind::SandboxUnavailable` first |
| `Unsandboxed`                                      | Commands always run without isolation, without a warning                   |

The warning is a `SandboxWarning` naming the `SandboxUnavailableReason`, a message, and
a remedy. A streamed run emits it as `RunEventKind::SandboxUnavailable`, and every
turn's `TurnReport::sandbox_warning` carries it, including `run_once`, `Harness::turn`,
and `Session::send`. Its message and remedy are written for the person using your
application, such as "Install bubblewrap", so you can show them as they are;
`SandboxUnavailableReason::remedy` documents what your own configuration can change
instead.

The workspace is read-only unless you grant `with_write`. Everything else is explicit
too:

| Grant                       | Native sandbox effect                                                                                                                                                          |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `with_system_reads`         | Reads the system directories Bash, Git, and toolchains need (`/usr`, `/System`, the Command Line Tools on macOS; `/usr`, `/lib*`, `/etc`, private `/proc` and `/dev` on Linux) |
| `with_read`                 | Reads one more directory, such as `~/.rustup` or `/opt/homebrew`                                                                                                               |
| `with_write`                | Writes beneath a workspace directory; `with_write(".")` grants the whole worktree                                                                                              |
| `with_temporary_directory`  | Reads and writes one directory outside the workspace, exported as `TMPDIR`; give each session its own                                                                          |
| `with_git_writes`           | Lets `git add` and `git commit` on one branch write the objects, that branch, and the worktree's own Git state; other references, configuration, and hooks stay read-only      |
| `with_network`              | Allows IP connections, listeners, and name resolution; denied by default                                                                                                       |
| `with_full_disk_read`       | Reads everything except hidden paths, as Claude Code and Codex do; always hides the keychain and other credential stores                                                       |
| `with_hidden_path`          | Hides one path, such as a project's `.env`, from reads and writes; refused inside a writable directory                                                                         |
| `with_writable_root`        | Reads and writes one directory outside the workspace, every home directory, and system directories                                                                             |
| `with_unix_socket`          | Allows connecting to one Unix-domain socket, such as `ssh-agent`'s, pinned when configured; macOS only, Linux warns                                                            |
| `with_excluded_command`     | Runs explicitly approved plain commands of one pinned program outside the sandbox, without a shell                                                                             |
| `with_excluded_environment` | Passes one environment value to excluded commands only                                                                                                                         |
| `with_environment`          | Passes one environment value                                                                                                                                                   |
| `with_host_information`     | Exposes host details such as `uname`; required                                                                                                                                 |

Grants are paths: the sandbox never walks a granted directory, so a toolchain-sized read
grant or a large `target/` or `node_modules` costs nothing per command. Links resolve to
their targets, which stay subject to the policy, so a link inside the workspace works
while one leading elsewhere cannot be followed. Hard links already inside a write grant
are files of that grant; commands cannot create new ones that reach outside it. On Linux
only a linked worktree can stage and commit under `with_git_writes`, which refuses a
write grant containing a Git directory, and only the repository's own Git metadata and a
repository at the top of each write grant stay read-only; macOS protects every `.git`
name. Replacement refs, object alternates, commit-graph files, grafts, and the shallow
boundary stay read-only too, since they change the history Git shows without changing
what it pushes. `with_git_writes(branch)` opens only that branch's reference and reflog
among the references: other branches, tags, `refs/stash`, and `packed-refs` stay
read-only, so creating a branch, stashing, or `git gc` fails with a lock error and
leaves the repository unchanged. Linux confines writes by directory, so there the
directory holding the branch stays writable: siblings of `wt/session` can still move,
but `main` cannot. Name session branches inside a directory, and keep checking other
branches in a host that reviews history. Commands can still point `HEAD` or the branch
at another branch, so check that `HEAD` resolves to the granted branch before the host's
own Git writes to the worktree. A command the sandbox refuses to start, for example
because a granted directory is missing, comes back to the model as a rejected tool call,
and the turn continues.

Coding sessions follow the sandboxes of Claude Code and Codex:

- `with_full_disk_read` always hides the keychains, browser profiles, and common
  credential stores (`~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.git-credentials`,
  `~/.npmrc`, Cargo's credentials, agent tokens, and more; the `with_full_disk_read`
  docs list them) under the account's home directory and any different `HOME`, where
  variables such as `XDG_CONFIG_HOME`, `CARGO_HOME`, `GNUPGHOME`, or `KUBECONFIG` move
  them, plus `/run/secrets`, and the network grant never reaches the keychain service.
  It also hides every other entry beside the session's temporary directory, so give each
  session its own directory inside a dedicated parent; a parent that contains the
  workspace or a home directory is refused, as is full-disk read when the account has no
  home directory. Pass tools a credential such as `GH_TOKEN` with `with_environment`;
  never grant the keychain.
- A writable root lets commands plant code that host tools run later, so it is refused
  inside any home directory, even for caches: Cargo, Go, and Git use cached entries
  without re-verifying them. Point tool caches at the session's temporary directory
  through the environment instead (`CARGO_HOME`, `npm_config_cache`, `GOMODCACHE`,
  `GOCACHE`, `PIP_CACHE_DIR`, and similar).
- An excluded program runs unsandboxed only when the `ApprovalHandler` decides
  `Decision::AllowUnsandboxed` on a call marked `ToolEffects::ExecutesUnsandboxed`;
  `Allow` keeps it sandboxed. The harness runs the pinned executable directly with the
  exact argument vector `ApprovalRequest::command` shows, with no shell, and only for
  plain printable-ASCII words without quotes, globs, `~`, `$`, or operators. It gets a
  minimal environment the host controls: a sanitized `PATH`, `HOME`, `USER`, `LOGNAME`,
  `LANG`, `LC_*`, and `TERM` from the harness, and `with_excluded_environment` values,
  never the sandbox's values or temporary directory. Excluding a program trusts it
  fully: it runs whatever its arguments name and follows configuration in the workspace,
  such as a `compose.yaml` or `.cargo/config.toml`, which the approval does not show.
  The model-facing `bash` description names excluded programs.
- A sandboxed command that fails with an operating-system denial on stderr, such as
  `Operation not permitted`, gets a hedged `sandbox` note in its result, and its
  `ToolCallItem` sets `likely_sandbox_denial`. The guess is advisory and the command's
  own output can trigger it: never rerun a command outside the sandbox automatically.
- Host executors receive hidden paths only by declaring
  `BashExecutor::supports_hidden_paths`.

Upgrading `ag-harness` can change what the native sandbox enforces, as this release does
for the macOS network grant (TLS trust evaluation added, the keychain service removed):
change your policy revision when you upgrade. Enforcing a grant or a denial is the
selected executor's job. Long output keeps each stream's start and end within the
capture budget (`capture_bytes`, up to `BashConfig::MAX_CAPTURE_BYTES`; hosts usually
pass `BashConfig::DEFAULT_CAPTURE_BYTES`). The budget caps captured bytes only: `bash`
results are not deferred by the per-round result share, so a larger budget spends more
of the context window per command. The omitted byte counts are on the `CommandOutcome`.
Command intents are recorded before spawning; await `settled()` (and
`retry_settlement()` after failures) so unresolved commands never block new turns
silently.

## Models

For any built-in provider selected at runtime, use `provider::ModelConfiguration`, which
reads the provider's key and endpoint through an injected environment lookup:

```rust
use ag_harness::model::ModelClient;
use ag_harness::provider::{KIMI_K3, ModelConfiguration, ModelProvider};

fn kimi() -> Result<ModelClient, Box<dyn std::error::Error>> {
    Ok(ModelConfiguration::new(ModelProvider::Kimi, KIMI_K3)
        .client_from_environment(|name| std::env::var(name))?)
}
```

`ModelProvider::OpenAiCompatible` serves any model of a local or self-hosted
OpenAI-compatible server (Ollama, llama.cpp, LM Studio, vLLM). Its API key
(`OPENAI_COMPATIBLE_API_KEY`) is optional and its base URL
(`OPENAI_COMPATIBLE_BASE_URL`) defaults to Ollama's `http://127.0.0.1:11434/v1`. The
harness cannot know such a model, so `ModelProvider::declared_profile` returns the
profile the environment declares (`OPENAI_COMPATIBLE_CONTEXT_WINDOW`, default 32,768;
`OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS`, default a quarter of the window, at most 8,192)
under the default estimator with a 50% margin. `ModelProvider::local_server` returns a
`LocalServer` for that endpoint, which lists the served models
(`LocalServer::inventory`) and reports the window the server runs a model with
(`LocalServer::running_context_window`, loading an unloaded Ollama model first);
`ModelProvider::running_profile` sizes the profile to that window, capping a larger
declared one:

```rust
use ag_harness::provider::{ModelConfiguration, ModelProvider};

async fn local_model() -> Result<(), Box<dyn std::error::Error>> {
    let local = ModelConfiguration::new(ModelProvider::OpenAiCompatible, "qwen3:latest")
        .client_from_environment(|name| std::env::var(name))?;
    let server = ModelProvider::OpenAiCompatible
        .local_server(|name| std::env::var(name))?
        .expect("OpenAI-compatible models have a local server");
    let profile = match server.running_context_window("qwen3:latest").await? {
        Some(window) => ModelProvider::OpenAiCompatible
            .running_profile(|name| std::env::var(name), window)?,
        None => ModelProvider::OpenAiCompatible
            .declared_profile("qwen3:latest", |name| std::env::var(name))?,
    }
    .expect("OpenAI-compatible models declare their profile");

    Ok(())
}
```

Its requests carry only portable fields plus the quirks local servers need: the schema
is stated in the prompt (with the `json_schema` response format when no tools are
offered), the output bound is sent as the legacy `max_tokens`, turn instructions join
the preceding user message for chat templates that require alternating roles, reasoning
is neither requested nor replayed, and no prompt cache key or image input is sent.
Requests stream and ask for streamed usage with `stream_options.include_usage`;
`with_stream_usage(false)` turns that off for a server that rejects it. The server must
support tool calls. A server that supports more opts in through
`ProviderSpec::openai_compatible(config)` and `ModelClient::from_spec`, such as
`with_image_input(true)` for a vision model or `JsonSchema` structured output for vLLM.

`ModelProvider::OpenAi` serves `OpenAI` models such as `GPT_6_1_SOL` through the
Responses API with `OPENAI_API_KEY` and an optional `OPENAI_BASE_URL` (default
`https://api.openai.com/v1`). Requests are streamed and stateless (`store: false`): each
resends its history, encrypted reasoning from earlier responses is replayed verbatim
while events and transcripts show only its summary text, and output is constrained by a
native JSON Schema format. `declared_profile` returns each cataloged model's documented
window under the default estimator with a 50% margin, as it does for the Qwen flash
models. For the Muse and Kimi models it returns the documented window under an estimator
fitted to the provider's reported tokens, and `None` for other models and the remaining
Qwen models, whose hosts declare their own. Kimi defaults to
`https://api.moonshot.ai/v1` (override with `KIMI_BASE_URL`):

```rust
use ag_harness::provider::{GPT_6_1_SOL, ModelConfiguration, ModelProvider};

fn openai() -> Result<(), Box<dyn std::error::Error>> {
    let openai = ModelConfiguration::new(ModelProvider::OpenAi, GPT_6_1_SOL)
        .client_from_environment(|name| std::env::var(name))?;
    let profile = ModelProvider::OpenAi
        .declared_profile(GPT_6_1_SOL, |name| std::env::var(name))?
        .expect("cataloged OpenAI models declare their profile");

    Ok(())
}
```

`ModelProvider::ChatGpt` serves the same Responses API billed to the user's ChatGPT plan
through OpenAI's "Sign in with ChatGPT". Instead of an API key it reads the sign-in
stored in a credential file the host chooses with `ModelConfiguration::auth_file`, else
the one `CHATGPT_AUTH_FILE` names; the harness has no default location. A host writes it
with `ChatGptSignIn::from_environment_with_auth_file(environment, auth_file)` or
`ChatGptLogin`, and `ModelCatalog::from_environment_with_auth_file` catalogs its models
from it. `chatgpt_auth_file_in(environment, data_dir)` locates the file under a host's
data directory, so `ag-harness login chatgpt` and Agentty share
`~/.agentty/auth/chatgpt.json`. `client_from_environment` fails with
`ModelConfigurationError::SignIn` until the user signs in, and each request refreshes
the one-hour access token shortly before it expires, serializing rotating refreshes
across processes with a lock file. Its models report `openai_chatgpt` as their provider,
so resumed sessions keep plan billing.

### Custom providers

`provider::ProviderSpec` describes any OpenAI-compatible Chat Completions, Responses, or
Anthropic Messages provider as data, and `ModelClient::from_spec` serves one of its
models without new code. A specification names its `WireProtocol`, base URL, `Auth`
(none, a bearer key, a named header, or a refreshing `CredentialSource`), extra headers
and query parameters, and request policy: structured-output mode, reasoning mapping,
reasoning-text replay, output-bound field, image input, prompt cache key, and
context-overflow markers. Its `ReasoningFamily` decides which recorded turns its models
replay with their provider reasoning intact after a model switch; other turns replay
their reasoning as short notes. `ProviderSpec::new` starts from the standard request for
the protocol with no optional capabilities, bounding Chat Completions output with the
current `OpenAI` field `max_completion_tokens`; a server that ignores it needs
`with_max_tokens_field(Some(MaxTokensField::MaxTokens))`. Provider quirks, such as that
legacy field or turn instructions joined to the preceding user message
(`with_instruction_placement(InstructionPlacement::JoinPreviousUser)`), are opt-ins that
each Chat Completions preset enables for its provider. Keys are literal values the host
resolved, so the harness reads no environment variables for a specification. The
built-in providers are presets of the same type, such as `ProviderSpec::kimi`, behind
their family's `providers-*` feature, and the `ModelClient` constructors send exactly
what their preset does. A preset serves its configured model under a policy tailored to
it; a custom specification names its model with `ProviderSpec::for_model`:

```rust
use ag_harness::model::ModelClient;
use ag_harness::provider::{Auth, ProviderSpec, WireProtocol};

fn azure_openai() -> Result<ModelClient, Box<dyn std::error::Error>> {
    let spec = ProviderSpec::new(
        "azure_openai",
        WireProtocol::Responses,
        "https://example.openai.azure.com/openai/v1",
    )
    .with_auth(Auth::Header {
        name: "api-key".to_string(),
        value: std::env::var("AZURE_OPENAI_API_KEY")?,
    })
    .with_query("api-version", "preview")
    .for_model("gpt-6-luna");

    Ok(ModelClient::from_spec(spec)?)
}
```

`ProviderSpec::with_retry` sets the specification's `RetryPolicy`. The default, which
every preset keeps, retries a `429` up to five times and one connection failure, waiting
a backoff from one second doubled up to five, or a longer `Retry-After` (delta-seconds
or an HTTP date, capped at sixty seconds). `with_retried` opts in server errors (`500`,
`502`-`504`) and an overloaded `529`; other builder methods set the attempts, delays,
caps, and `Jitter`. A spent quota and a timed-out request are never retried, and
cancelling a run stops a pending wait.

The provider name, here `azure_openai`, is what telemetry, persisted sessions, and
provider errors record, and a built-in provider name keeps its built-in
`ReasoningFamily` unless `ProviderSpec::with_reasoning_family` replaces it.
`ModelClient::from_spec` runs `ProviderSpec::validate` and fails with
`ProviderSpecError::Invalid`, naming the field, for a specification no request can be
sent with: a base URL that is not an absolute `http(s)` URL without credentials or a
query, a blank key, an invalid header or one the harness sets itself, a blank effort
name or overflow marker, or an option its protocol cannot express. It fails with
`ProviderSpecError::MissingModel` for a specification that names no model. Header and
query values are sent as secrets: debug output and errors never print them, nor
credentials or a query in the base URL, and requests follow no redirect.
`ModelClient::from_spec` declares no context profile, even for a preset whose
constructor, such as `ModelClient::kimi`, attaches the catalog's, so hosts set the
budget explicitly.

### Claude through the Anthropic Messages API

`ModelProvider::Anthropic` serves the cataloged Claude models (`CLAUDE_FABLE_5_2`,
`CLAUDE_OPUS_5_5`, `CLAUDE_SONNET_5_5`, and `CLAUDE_HAIKU_5_5`) with `ANTHROPIC_API_KEY`
and an optional `ANTHROPIC_BASE_URL` (default `DEFAULT_ANTHROPIC_BASE_URL`), which takes
the versioned URL or, as Anthropic's SDKs read it, the origin alone, to which
configuration appends `/v1`. Each declares its documented 1,000,000-token window and
128,000-token output limit, and built-in prices with Anthropic's cache-read and 5-minute
cache-write rates. Claude Haiku 5.5 also declares its fivefold prices for a prompt over
100,000 tokens (`Pricing::with_long_context`), which `Cost::of` applies per request:

```rust
use ag_harness::model::ModelClient;
use ag_harness::provider::{CLAUDE_OPUS_5_5, ModelConfiguration, ModelProvider};

fn claude() -> Result<ModelClient, Box<dyn std::error::Error>> {
    Ok(ModelConfiguration::new(ModelProvider::Anthropic, CLAUDE_OPUS_5_5)
        .client_from_environment(|name| std::env::var(name))?)
}
```

`ModelClient::anthropic` builds the same client from an `AnthropicConfig` (the API key,
a base URL, and the model). Underneath, `WireProtocol::AnthropicMessages` serves Claude
models and `ProviderSpec::anthropic` presets it, sending the key as `x-api-key` beside
the protocol's `anthropic-version: 2023-06-01` header, which `ProviderSpec::with_header`
can replace. Serve an uncataloged Claude model or a gateway with
`ModelClient::from_spec`, declaring its context profile with a `ModelRegistry`
registration's `ModelCapabilities` (see "Model catalog" below) and its prices with
`catalog::ModelSpec::with_pricing`.

Requests stream, so thinking, text, and tool arguments arrive as `ItemDelta` events, and
mark the tools, `system` blocks, and latest message as prompt-cache breakpoints
(`ProviderSpec::with_cache_breakpoints(false)` turns that off). Context reductions never
alter the signed thinking of the round a tool loop continues. Structured output uses the
native `output_config.format`; `StructuredOutputMode::ForcedTool` instead reads the
answer from a `structured_output` tool call, for models or schemas native output
rejects. It forces that tool only for a specification without a reasoning mapping, since
thinking, which current Claude models do by default, forbids a forced tool. A reasoning
effort turns on summarized adaptive thinking for the models that take one. Signed
`thinking` and `redacted_thinking` blocks are recorded and replayed verbatim in their
original place on later requests, and earlier turns replay with their turn instructions
as first sent (`Model::replays_turn_instructions`), because the newest Claude models
drop or reject signed thinking after an edited earlier message. Context reductions that
clear or elide earlier rounds still edit history. Where Anthropic enforces the check, a
replay after such an edit fails with a `400` unless the request sets
`thinking.block_binding.prefix_mismatch_behavior: "drop_block"` under the
`thinking-binding-controls-2026-08-01` beta, and setting it opts an unenforced account
into the check. The preset therefore sets it on every request that sends thinking only
for the models Anthropic enforces for every account (Claude Fable 5.2, Mythos 5.1 and
5.2). For the models it enforces only for accounts created on or after 2026-08-31
(Claude Fable 5.1, Opus 5.5, Sonnet 5.5, and Haiku 5.5), requests leave it unset, so
older accounts keep earlier thinking, and a request rejected for an invalidated block is
sent once more with it. Either way the API then drops that thinking for the request, so
long sessions can lose it. A request without an effort that replays thinking sends the
thinking object only for the models that run the check. Every Claude model shares one
reasoning family, and a switch to another provider carries that reasoning as a short
note. The API requires an output bound, so a request without one sends `max_tokens`
128,000, the output limit, for a cataloged model through `ModelClient::anthropic`, and
otherwise 16,000 (`ProviderSpec::with_fallback_max_tokens`). Usage counts cache reads
and cache writes as input, and `Usage::cache_write` reports the writes. A `refusal` or
`max_tokens` stop fails as an incomplete response, a `model_context_window_exceeded`
stop or a "prompt is too long" `400` as a context overflow, and an HTTP `529` as
`ErrorKind::Overloaded`.

`ModelProvider::pricing` returns each per-token model's list prices as
`catalog::Pricing` in integer micro-USD per million tokens, and
`ModelProvider::capabilities` its image, reasoning, native structured output, and tool
support. `ModelProvider::cost_tier` derives each cataloged model's `CostTier` from those
prices (`Cheap` up to $2.50 per million output tokens, `Premium` from $15, `Standard`
between), with `Plan` for every ChatGPT plan model, so a host can route simple tasks to
a cheap model. An OpenAI-compatible server's price depends on where it runs:
`ModelProvider::cost_tier` returns `None` for it, as for an uncataloged model, and
`ModelProvider::cost_tier_at(model, base_url)` returns `Local` for a local or
private-network endpoint and `None` for a hosted one, such as OpenRouter.
`ModelProvider::trains_on_prompts` flags tiers, such as `muse-spark-1.3-contributor`,
that let the provider train on requests:

```rust
use ag_harness::provider::{CostTier, ModelProvider, QWEN3_8_FLASH};

assert_eq!(
    ModelProvider::Qwen.cost_tier(QWEN3_8_FLASH),
    Some(CostTier::Cheap)
);
```

### Model catalog

A `ModelCatalog` lets a host select models by identifier, role, or capability. Each
`catalog::ModelSpec` records a model's provider, context profile, output limit,
`Capabilities`, optional `Pricing`, and optional concurrency limit. Harnesses built with
`HarnessBuilder::catalog` share each model's `max_concurrency` across all their sessions
and runs: it limits model requests in flight, not runs, a request reserves budget only
once it holds a permit, and a cancelled request releases its permit. A `Role` is an open
set (`Role::SMART`, `CHEAP`, `FAST`, `VISION`, or any `Role::new("review")`) mapped to
ordered candidates; `CatalogBuilder::require` sets the capabilities every candidate of a
role must have (`Role::VISION` requires image input), and `ModelSelector::Require` picks
the first candidate with further needed capabilities. `ModelCatalog::from_environment`
catalogs every built-in model whose provider the environment configures, keyed
`provider/model`, except tiers that train on prompts. Its default role candidates,
documented on `ModelCatalog`, are only models with a declared context profile;
`into_builder` changes them, and `CatalogBuilder::pricing` sets host prices:

```rust
use ag_harness::Harness;
use ag_harness::catalog::{Capabilities, ModelCatalog, ModelSelector, Role};

fn vision_harness() -> Result<Harness, Box<dyn std::error::Error>> {
    let catalog = ModelCatalog::from_environment()?;
    let vision = catalog.resolve(&ModelSelector::Require {
        role: Role::SMART,
        needs: Capabilities::NONE.with_image_input(true),
    })?;

    Ok(Harness::builder().model(vision.model()).build()?)
}
```

The catalog is optional. `HarnessBuilder::catalog` with `model_selector` (default
`Role::SMART`) resolves the model at build time; an explicit `model` replaces the
catalog, and setting `model` together with `model_selector` or `summary_model_selector`
fails the build. When the model comes from the catalog, the catalog also resolves the
summary model, which writes compaction checkpoints and in-turn round summaries:
`summary_model_selector`, or by default the first `Role::CHEAP` model with structured
output that the session model's provider serves, so session content stays with that
provider, and that has prices when the session model has them. Host models share a
provider only when both report metadata naming the same provider. The summary model must
have structured output and accept the summary schema, or the build fails. Summaries stay
on the session model when nothing else resolves. An explicit `context_estimator` sizes
the summary model too, and an explicit `context_budget` does unless the summary model's
own profile declares a smaller one; otherwise its own profile sizes it. It asks for low
reasoning effort only when it declares reasoning. A round summary that does not fit the
summary model's budget goes to the session model. A request the summary model answers is
priced at its own `Pricing` and summed into the run's cost, reserves its own estimate
from the run's budget ledger, takes a permit of its own `max_concurrency`, and counts
toward `RunLimits` like any other request. A run with any request answered by an
unpriced model reports no cost, so a selected summary model without prices leaves the
runs it summarizes in unpriced. Each `ModelRequestCompleted` event and
`ModelRequestActivity` names the cataloged model that answered it, with its own cost.

A harness built from a catalog records no model registration, so its durable sessions
resume only on harnesses that record none, never through `Harness::from_registry`. Give
it `registration_revision` to record the selected entry as a registration keyed by its
`ModelId`, and register the entries a host resumes or switches sessions to with
`ModelRegistry::register_catalog_entry` under the same revision. Entries without a
context profile cannot be registered, so the example skips them:

```rust
use std::sync::Arc;

use ag_harness::Harness;
use ag_harness::catalog::{ModelCatalog, ModelSelector};
use ag_harness::model::ModelRegistry;

fn registered_catalog() -> Result<(Harness, ModelRegistry), Box<dyn std::error::Error>> {
    let catalog = Arc::new(ModelCatalog::from_environment()?);
    let harness = Harness::builder()
        .catalog(Arc::clone(&catalog))
        .registration_revision("config-v1")
        .sqlite("harness.db")
        .build()?;
    let mut models = ModelRegistry::new();
    for spec in catalog.specs().filter(|spec| spec.profile().is_some()) {
        let entry = catalog.resolve(&ModelSelector::Id(spec.id().clone()))?;
        models.register_catalog_entry(&entry, "config-v1")?;
    }

    Ok((harness, models))
}
```

Both record the key and revision a manual `register` with an equal `ExecutionIdentity`
records, and the tool calls, image input, and profile of the entry, so sessions recorded
on either resume on the other, and `Harness::from_registry` over a catalog registration
resumes sessions created by the catalog harness. A catalog registration also keeps its
entry's prices, `max_concurrency`, and output limit through `from_registry` and
`switch_model`. A harness given an explicit `model` cannot record one.

A harness built by `Harness::from_registry` has no catalog to resolve a summary model,
so it summarizes on its own model until `Harness::set_summary_model` names a catalog
entry for it. That entry must have structured output and accept the summary schema, or
the call fails and keeps the previous choice; it is sized, priced, and limited like a
selected summary model. The choice is not part of the recorded registration, so sessions
resume with or without it. `switch_model` drops it, and `Session::set_summary_model`
chooses one for the new model.

Built-in and specification-defined clients call `ag-router` for provider transport and
schema validation. Hosts can also use `ag-router` directly for structured chat calls
without a harness. Implement the `model::Model` trait to bring a provider whose wire
protocol no `ProviderSpec` describes into `ag-harness`.

`model::ModelRegistry` selects models by stable host keys with declared capabilities:

```rust
use ag_harness::Harness;
use ag_harness::model::{ModelCapabilities, ModelProfile, ModelRegistry};
use ag_harness::provider::{MUSE_SPARK_1_3, ModelConfiguration, ModelProvider};
use ag_harness::recovery::ExecutionIdentity;

fn review_harness() -> Result<Harness, Box<dyn std::error::Error>> {
    let mut models = ModelRegistry::new();
    models.register(
        ExecutionIdentity::new("review-model", "config-v1")?,
        ModelConfiguration::new(ModelProvider::Muse, MUSE_SPARK_1_3)
            .client_from_environment(|name| std::env::var(name))?,
        ModelCapabilities::new(ModelProfile::new(1_048_576).with_estimate_margin(131_072, 18))
            .with_image_input(true)
            .with_tool_calls(true),
    )?;

    Ok(Harness::from_registry(&models, "review-model")?)
}
```

- Capabilities are host declarations, not detection. The required `ModelProfile`
  declares the model's context window, reserved tokens, and estimator; registration
  derives the `ContextBudget` from it and rejects a profile that leaves no input
  capacity. The budget bounds replayed history as above and fails before any provider
  call when mandatory content cannot fit. `TurnReport::history` says what was replayed
  or dropped.
- `session.switch_model(&models, key)` changes the model of an idle session. It rejects
  tool or image history the target cannot replay with
  `SessionError::UnsupportedModelHistory`. Reasoning from a model of another reasoning
  family (`OpenAI` and `ChatGPT`-plan models share one) is replayed to the new model as
  short `[harness]` notes: as reasoning when the new model replays reasoning text, or as
  assistant text otherwise; tool calls keep their ids, arguments, and results. The
  stored history is unchanged, so switching back replays it natively. Every switch,
  including one between models of the same provider, loses the provider's prompt cache.
  `session.compact()` stores a schema-validated summary of the oldest uncovered turns
  that fit, which later turns replay first; call it again to cover the rest. It runs on
  the harness's summary model, and `session.compact_with(selector)` runs it on the model
  the selector resolves from the harness's catalog instead. A selector naming a model
  other than the session or summary model starts that model's calibration afresh on
  every call, so its sources are sized by its raw estimator. The checkpoint records the
  model that wrote it. A switched session compacts and summarizes on its registered
  model, and runs unpriced unless that model was registered from a catalog entry.
- `context_policy(ContextPolicy::auto_compact())` compacts automatically. By default
  (`ContextPolicy::Evict`) history that outgrows the weight left for it is re-projected
  and its oldest turns are evicted. With `auto_compact()` (90 percent) or
  `auto_compact_at(threshold_percent)`, a turn whose uncovered history passes that share
  first runs the same compaction on the summary model, leaving the newest turn
  uncovered, and then admits the turn against the new checkpoint. If compaction fails or
  history still does not fit, the turn evicts as before, and after a failure the
  in-memory `Session` handle waits a growing number of turns before trying again; the
  backoff is not stored, so keep a long-lived handle to benefit from it. A streamed run
  reports each attempt as `RunEventKind::Compacted`, whose `CompactionOutcome` is
  `Compacted`, `NothingToCover`, or `Failed`. Compaction requests lead the `TurnReport`
  as `ModelRequestPurpose::Compaction`, belong to the turn's lifecycle turn, reserve
  from the run's budget ledger, and count toward its limits and usage, even when the run
  is cancelled during compaction. They may spend all but one of the run's model requests
  and half its tokens; the ledger refusing a request, the wall-clock limit, or a store
  failure fails the turn rather than the compaction.
- Resuming with another registration fails with `SessionError::RegistrationMismatch`,
  which names the stored registration so a host can resume with it and switch.
- Bump the registration revision whenever configuration or credentials scope changes.
  Never put secrets in it. A revised `ModelProfile` alone needs no new revision: it only
  shapes how new requests are fitted, while a new revision would strand stored sessions.
  Sessions switched to a registration adopt its budget and estimator.

Built-in clients accept JSON wrapped in Markdown fences or prose when the embedded
object parses; anything else fails with `ModelError::InvalidJson`. Requests time out
after three minutes and retry under the provider's `RetryPolicy`. A provider rejection
of input beyond the model's window reports `ModelErrorType::ContextOverflow`
(`ModelError::is_context_overflow`); custom models report it with
`ModelError::context_overflow`.

## Errors

Use `RunError::from` for a turn's `TurnError`, `SessionError`, or `ModelError`, and
`HarnessError::from` for session operations and build-time errors. Both classify by one
`ErrorKind` and `is_retryable()`, keep the original message and source chain, and return
the original error through `get_ref()` and `into_inner()`. Only `RateLimited` (`429`),
`Overloaded` (`500`, `502`-`504`, `529`), `Transport`, and `Busy` (including a locked or
saturated store) are retryable; a retryable provider failure also carries its
`Retry-After` (delta-seconds or an HTTP date) as `retry_after()`, capped by the
provider's `RetryPolicy`, by default at sixty seconds. A spent quota (`402`, or a `429`
with OpenAI's `insufficient_quota` or Kimi's `exceeded_current_quota_error`) is
`QuotaExceeded`, not retryable, and not retried internally; `401`/`403` are `Auth`.
Custom models report the same classes with `ModelError::provider` and
`ModelError::quota_exceeded`. `TurnErrorType` stays the telemetry label.

## Stores

SQLite is the default (`Harness::database`, or `store::SqliteStore` explicitly).
`store::MemoryStore` gives resumable sessions within one process. Share one store
between harnesses as an `Arc<dyn store::SessionStore>` passed to `HarnessBuilder::store`
or `Harness::store`. `SessionStore` is sealed: its lease, journal, and record operations
are internal, so these two are the only implementations.

## Observability

`Harness::with_lifecycle_observer` receives turn, model, and tool events that never
contain prompts or output. `lifecycle::LifecycleMetrics` and
`lifecycle::LifecycleTraceObserver` export them to OpenTelemetry. Turn spans nest under
the caller's OpenTelemetry context, including for `.start()`ed turns.
`LifecycleTraceObserver::with_tool_attributes` repeats attributes a host derives from
the turn's context, such as run identity, on its `execute_tool` spans. A failed turn's
`LifecycleEventKind::TurnFailed` carries the `TurnReport` of what it did before failing:
its reported requests, tool calls, and counts.
`LifecycleEventKind::ModelRequestCompleted` carries the request's `Cost` at the
answering catalog model's prices, unless the model is unpriced or the provider omitted a
count, whether that cost is an upper bound because cache reads were priced at the input
price, and that model's context window; `chat` spans export them. Each fitted request
reports the calibrated capacity it was fitted into (`budget_weight`) and its weight
before (`unreduced_weight`) and after (`estimated_weight`) reduction.

## Features

The default features build every backend. Turn them off with `default-features = false`
to keep only the core: `MemoryStore`, host-implemented `Model`s, and host-selected Bash
executors such as `UnsandboxedExecutor`.

| Feature               | Default | Enables                                                                                   |
| --------------------- | ------- | ----------------------------------------------------------------------------------------- |
| `sqlite`              | yes     | `store::SqliteStore`, `Harness::database`, `HarnessBuilder::sqlite`, bundled migrations   |
| `native-sandbox`      | yes     | The native Bash sandbox, `sandbox::dispatch`, and the `ag-harness-sandbox` helper         |
| `otel`                | yes     | OpenTelemetry request metrics, `LifecycleMetrics`, `LifecycleTraceObserver`, span context |
| `providers-openai`    | yes     | `OpenAI` Responses API clients and `ChatGPT` plan sign-in                                 |
| `providers-compat`    | yes     | Chat Completions clients: Kimi, Qwen, Muse, and local or self-hosted servers              |
| `providers-anthropic` | yes     | `ModelClient::anthropic` and the `ProviderSpec::anthropic` preset for Claude models       |

Any provider feature builds the `provider` module, `ModelClient`, and `ProviderSpec`
with `ModelClient::from_spec` for every wire protocol, and
`ModelCatalog::from_environment` with every cataloged provider's entries. Each
`ProviderSpec` preset needs its family's feature. `ModelProvider::is_enabled` reports
whether the build includes a provider's family; a client whose family is off fails with
`ModelConfigurationError::ProviderDisabled` before any credential is read, and
`ModelCatalog::from_environment` skips it. Without `native-sandbox`, `BashConfig::new`,
`BashConfig::native`, `HarnessBuilder::sandbox`, and `sandbox::dispatch` are absent and
Bash requires `BashConfig::for_executor`. Without `sqlite`, durable sessions require
`Harness::store`. Features are additive: error enums whose variants depend on them are
`#[non_exhaustive]`.

## Where things live

The crate root holds what a typical host needs: `Harness`, `Session`, `TurnOptions`,
`TurnOutcome`, `TurnError`, `SessionError`, `RunError`, `HarnessError`, `ErrorKind`,
`OutputSchema`, `ToolSet`, `ToolHandler`, `ApprovalHandler`, `Decision`, `Repository`,
`TurnInput`, `Model`, `ModelCatalog`, `ModelSelector`, and `Role`. Everything else is
grouped by topic:

| Module       | Contents                                                 |
| ------------ | -------------------------------------------------------- |
| `provider`   | Muse, Kimi, Qwen, OpenAI, Claude, and compatible clients |
| `catalog`    | Model specs, roles, capabilities, pricing, and selection |
| `model`      | The `Model` trait, requests, registry, context budgets   |
| `tool`       | Tool arguments, results, and the `FileSystem` seam       |
| `bash`       | Bash configuration, executors, enforcement, records      |
| `sandbox`    | Native sandbox choice, helper entry point, warnings      |
| `turn`       | Turn builders, reports, activity, and settlement errors  |
| `store`      | Session stores and durable write and checkpoint records  |
| `recovery`   | Execution identities and idempotent host request records |
| `transcript` | Settled-turn transcripts and their wire shape            |
| `lifecycle`  | Content-free events and OpenTelemetry observers          |

The API docs on each type carry the detailed contracts. The
[design page](https://agentty.xyz/docs/architecture/ag-harness-design/) covers runtime
and persistence boundaries.

## API stability

Public enums are `#[non_exhaustive]`, and so are the struct variants of events, reports,
and errors the crate builds (`RunEventKind`, `LifecycleEventKind`, `ToolActivity`,
`TurnError`), so match them with a wildcard arm and `..`. Records the crate builds, such
as `CommandRecord`, `WriteRecord`, and `CompletionUsage`, gain fields through `with_`
setters or accessors; `ModelError` variants stay open for custom models to construct,
and provider `XxxConfig` structs stay open for hosts to fill in.

### Upgrading from 0.20

The release that removes the deprecated items below is a minor version bump.

| Removed                                                         | Use instead                                                                                   |
| --------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `Tool`, `ToolPolicy`, `Harness::allow`                          | `ToolSet::empty().with_read()` (and so on) passed to `Harness::tools`                         |
| `HarnessBuilder::allow`                                         | `HarnessBuilder::tools` with a `ToolSet`                                                      |
| `TurnOptions::new(output, policy)`                              | `TurnOptions::from_output(output).with_tools(set)`                                            |
| `TurnOptions::tool_policy`                                      | `TurnOptions::tools`                                                                          |
| `RunRequest::tool_policy`                                       | `RunRequest::tools`                                                                           |
| `MalformedArguments::tool`                                      | `MalformedArguments::name`                                                                    |
| `SessionError::{Migration, QueryContext}`                       | `SessionError::Store`: match `HarnessError::kind`, or downcast its source                     |
| Positional `CompletionUsage::new` and `CompletionMetadata::new` | `CompletionUsage::default()` or `CompletionMetadata::new(finish_reason)` with `with_` setters |
| `ModelCapabilities { .. }` literals                             | `ModelCapabilities::new(profile)` with `with_image_input`, `with_tool_calls`                  |
| Implementing `store::SessionStore`                              | No replacement yet: use `MemoryStore` or `SqliteStore`                                        |
