# `ag-harness`

Structured LLM turns with deny-by-default tools and durable sessions.

- **Every answer is typed.** Each turn ends in JSON validated locally against your
  schema.
- **Tools are off until you allow them.** Read, write, and sandboxed Bash each need an
  explicit grant.
- **Your store owns the conversation.** Sessions resume from SQLite (or your own store)
  in any process; the provider never holds the only copy.

## Quickstart

```rust
use std::env;
use std::sync::Arc;

use ag_harness::provider::{GPT_6_LUNA, ModelConfiguration, ModelProvider};
use ag_harness::{Harness, OutputSchema};
use serde_json::json;

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
```

Every harness needs a `ContextBudget`: the approximate request weight the model's
context window allows, in roughly token-sized units. The builder derives it from the
model's declared `ModelProfile`: built-in `OpenAI`, `ChatGPT`, and Qwen flash models
declare one. For other models, set it with `context_budget`. `build` checks only the
model, the budget, and that allowed repository tools have a repository, and reports a
failure as a `HarnessError`; storage opens on first session use. `run_once` keeps no
history. For a conversation, use a session.

A `Harness` is a cheap, `Clone + Send + Sync` handle: clones share one configuration and
one SQLite connection pool, so one harness can serve many concurrent sessions and turns.
Separate harnesses or processes may share a SQLite file once it exists, but let one of
them open a new file first: concurrent first opens can race on schema migration.
`Harness::new` and `Harness::from_registry` remain available.

## Sessions

```rust
use ag_harness::{Harness, Repository, Tool};

let harness = Harness::builder()
    .model(model)
    .context_budget(budget)
    .sqlite("harness.db")
    .repository(Repository::new(".", git_executable)?)
    .allow(Tool::Read)
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
```

Every request replays finished turns within the context budget and the history byte
limit; the provider never holds conversation state. An older turn may replay with its
tool results, reasoning, and `write` patches replaced by short notices, or else as its
input and final answer, and a turn larger than eight times the byte limit loads only as
its input and final answer. Those forms are sticky: each turn repeats the history the
previous turn sent and appends that turn, so provider prompt caches keep the prefix, and
only history that outgrows a limit is shortened, newest first, to half of it. Each model
response and tool result is stored as soon as it finishes. A failed or interrupted turn
replays what it recorded plus a note naming why it stopped and any writes or commands
whose outcome is unknown; when it does not fit, it replays with its tool traffic
replaced by notices, or else as only its input and note. Sessions run concurrently, with
one active turn per session. The library never picks a database location for you.

## Turns

`run_once` and `send` run a turn with the defaults. When a turn needs more, call `turn`
and chain what you need before awaiting it:

```rust
let outcome = session
    .turn("Review against main")
    .options(TurnOptions::new(schema, ToolPolicy::default().allow(Tool::Read)))
    .host_id("request-7")
    .await?;
```

| Need                                    | Chain                                       |
| --------------------------------------- | ------------------------------------------- |
| Different schema or tools               | `.options(TurnOptions::new(...))`           |
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
  over every model request the run completed, even when it failed or was cancelled, the
  run's `files_changed` and `commands`, and its `report`. Cleanup that still fails is
  reported in `settlement`, whose `Unsettled::retry()` readmits the session; it never
  replaces `result`. A cloneable `CancelToken` from `run.cancel_token()`, or
  `into_handle()`, cancels it from any task. `harness.run` keeps its single turn in a
  fresh in-memory session and requires `RunRequest::output`.
- **Runs stream typed events.** `run.into_parts()` returns an `EventStream` (a
  `futures::Stream` of `RunEvent`) and a `RunHandle`. Each event has the run ID, a
  gap-free `seq`, and a timestamp. Events report the admitted turn, assistant messages,
  reasoning, tool calls with their arguments and results, per-request usage, and context
  reductions. `Completed` is always the last event, also after a failure or
  cancellation. Delivery is lossless: the run waits while the stream is full, so read it
  while awaiting the outcome. Dropping the stream discards events without stalling the
  run. Events carry session content, so never export them as telemetry. Every type
  derives `serde` and `schemars::JsonSchema`; the schema is committed in
  `crates/ag-harness/schema/run_event.json`.
- **Turns have no tool-call limit.** Cancellation and the `ContextBudget` bound a turn.
  Within a turn, requests only append, so they share one cached prefix, until tool
  traffic grows a request past the budget. That request is sent as a reduced copy
  without an extra model call, reduced deeply once so several later requests append to
  it unchanged. First, older tool results that a later call superseded (a re-read of the
  same target, an applied write to the file read, or a rerun of the same command) become
  notices naming that call. Then the new round's results above 25% of the input budget
  are cut (a `read` result by whole lines or items, any other to a head and tail
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
  reasoning effort. Invalid output is corrected like a final output; a failed summary
  falls back to the steps above, and the turn stops asking after two failures. Summary
  requests count toward the turn's model requests
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
  Muse and Kimi, which route requests sharing a key to the backend holding their cached
  prefix, and leave it off for Qwen. Checkpoint and round-summary requests carry none.
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

| Tool          | Does                               | Also requires                                 |
| ------------- | ---------------------------------- | --------------------------------------------- |
| `Tool::Read`  | Read, list, search, and show files | A `Repository`                                |
| `Tool::Write` | Apply one bounded unified diff     | A `Repository`                                |
| `Tool::Bash`  | Run a command in a sandbox         | `TurnOptions::with_bash(BashConfig)` per turn |

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

`BashConfig::new` selects the native sandbox: Bubblewrap, Landlock, and seccomp on
Linux; Seatbelt on macOS. Install the matching `ag-harness-sandbox` binary outside the
workspace and pass its absolute path plus a trusted Bash. `BashConfig::for_executor`
selects your own `BashExecutor` instead, such as
`UnsandboxedExecutor::without_isolation` for hosts already inside a container or VM.
There is never a silent fallback. Each executor reports its `Enforcement` (none,
filesystem, or filesystem and network), and the `bash` tool description tells the model
exactly that.

The workspace is read-only unless you grant `with_write`. External reads (`with_read`),
environment values (`with_environment`), and host details (`with_host_information`) are
explicit too. Networking is never granted; enforcing that, like every grant, is the
selected executor's job. Long output keeps each stream's start and end within the
capture budget (`capture_bytes`, up to `BashConfig::MAX_CAPTURE_BYTES`; hosts usually
pass `BashConfig::DEFAULT_CAPTURE_BYTES`). The budget caps captured bytes only: `bash`
results are not deferred by the per-round result share, so a larger budget spends more
of the context window per command. The omitted byte counts are on the `CommandOutcome`.
Command intents are recorded before spawning; await `settled()` (and
`retry_settlement()` after failures) so unresolved commands never block new turns
silently.

## Models

`provider::Muse::from_env` builds the Muse client from environment variables. For any
built-in provider selected at runtime, use `provider::ModelConfiguration`:

```rust
use ag_harness::provider::{KIMI_K3, ModelConfiguration, ModelProvider};

let kimi = ModelConfiguration::new(ModelProvider::Kimi, KIMI_K3)
    .client_from_environment(|name| std::env::var(name))?;
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
```

Its requests carry only portable fields: the schema is stated in the prompt (with the
`json_schema` response format when no tools are offered), reasoning is neither requested
nor replayed, and no prompt cache key or image input is sent. The server must support
tool calls.

`ModelProvider::OpenAi` serves `OpenAI` models such as `GPT_6_1_SOL` through the
Responses API with `OPENAI_API_KEY` and an optional `OPENAI_BASE_URL` (default
`https://api.openai.com/v1`). Requests are streamed and stateless (`store: false`): each
resends its history, encrypted reasoning from earlier responses is replayed verbatim,
and output is constrained by a native JSON Schema format. `declared_profile` returns
each cataloged model's documented window under the default estimator with a 50% margin,
as it does for the Qwen flash models, and `None` for other models and the remaining
Muse, Kimi, and Qwen models, whose hosts declare their own:

```rust
use ag_harness::provider::GPT_6_1_SOL;

let openai = ModelConfiguration::new(ModelProvider::OpenAi, GPT_6_1_SOL)
    .client_from_environment(|name| std::env::var(name))?;
let profile = ModelProvider::OpenAi
    .declared_profile(GPT_6_1_SOL, |name| std::env::var(name))?
    .expect("cataloged OpenAI models declare their profile");
```

`ModelProvider::ChatGpt` serves the same Responses API billed to the user's ChatGPT plan
through OpenAI's "Sign in with ChatGPT". Instead of an API key it reads the sign-in
stored at `chatgpt_auth_file` (`CHATGPT_AUTH_FILE`, else `auth/chatgpt.json` under
`AGENTTY_ROOT` or `~/.agentty`), written by `ag-harness login chatgpt` or by a host
through `ChatGptLogin`. `client_from_environment` fails with
`ModelConfigurationError::SignIn` until the user signs in, and each request refreshes
the one-hour access token shortly before it expires, serializing rotating refreshes
across processes with a lock file. Its models report `openai_chatgpt` as their provider,
so resumed sessions keep plan billing.

`ModelProvider::pricing` returns each per-token model's list prices as
`catalog::Pricing` in integer micro-USD per million tokens, and
`ModelProvider::capabilities` its image, reasoning, native structured output, and tool
support. `ModelProvider::cost_tier` derives each cataloged model's `CostTier` from those
prices (`Cheap` up to $2.50 per million output tokens, `Premium` from $15, `Standard`
between), with `Plan` for every ChatGPT plan model and `Local` for every
OpenAI-compatible model, so a host can route simple tasks to a cheap model.
`ModelProvider::trains_on_prompts` flags tiers, such as `muse-spark-1.3-contributor`,
that let the provider train on requests:

```rust
use ag_harness::provider::{CostTier, QWEN3_8_FLASH};

assert_eq!(
    ModelProvider::Qwen.cost_tier(QWEN3_8_FLASH),
    Some(CostTier::Cheap)
);
```

### Model catalog

A `ModelCatalog` lets a host select models by identifier, role, or capability. Each
`catalog::ModelSpec` records a model's provider, context profile, output limit,
`Capabilities`, optional `Pricing`, and optional concurrency limit. A `Role` is an open
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

let catalog = ModelCatalog::from_environment()?;
let vision = catalog.resolve(&ModelSelector::Require {
    role: Role::SMART,
    needs: Capabilities::NONE.with_image_input(true),
})?;
let harness = Harness::builder().model(vision.model()).build()?;
```

The catalog is optional. `HarnessBuilder::catalog` with `model_selector` (default
`Role::SMART`) resolves the model at build time; an explicit `model` replaces the
catalog, and setting both `model` and `model_selector` fails the build.

Built-in clients call `ag-router` for provider transport and schema validation. Hosts
can also use `ag-router` directly for structured chat calls without a harness. Implement
the `model::Model` trait to bring any other provider into `ag-harness`.

`model::ModelRegistry` selects models by stable host keys with declared capabilities:

```rust
use ag_harness::model::{ModelCapabilities, ModelProfile, ModelRegistry};
use ag_harness::recovery::ExecutionIdentity;

let mut models = ModelRegistry::new();
models.register(
    ExecutionIdentity::new("review-model", "config-v1")?,
    Muse::from_env(MUSE_SPARK_1_3)?,
    ModelCapabilities {
        image_input: true,
        tool_calls: true,
        ..ModelCapabilities::new(ModelProfile::new(1_048_576).with_estimate_margin(131_072, 18))
    },
)?;
let harness = Harness::from_registry(&models, "review-model")?;
```

- Capabilities are host declarations, not detection. The required `ModelProfile`
  declares the model's context window, reserved tokens, and estimator; registration
  derives the `ContextBudget` from it and rejects a profile that leaves no input
  capacity. The budget bounds replayed history as above and fails before any provider
  call when mandatory content cannot fit. `TurnReport::history` says what was replayed
  or dropped.
- `session.switch_model(&models, key)` changes the model of an idle session.
  `session.compact()` stores a schema-validated summary of the oldest uncovered turns
  that fit, which later turns replay first; call it again to cover the rest.
- Bump the registration revision whenever configuration or credentials scope changes.
  Never put secrets in it. A revised `ModelProfile` alone needs no new revision: it only
  shapes how new requests are fitted, while a new revision would strand stored sessions.
  Sessions switched to a registration adopt its budget and estimator.

Built-in clients accept JSON wrapped in Markdown fences or prose when the embedded
object parses; anything else fails with `ModelError::InvalidJson`. Requests time out
after three minutes, and a `429` `Retry-After` is honored up to sixty seconds. A
provider rejection of input beyond the model's window reports
`ModelErrorType::ContextOverflow` (`ModelError::is_context_overflow`); custom models
report it with `ModelError::context_overflow`.

## Errors

Use `RunError::from` for a turn's `TurnError`, `SessionError`, or `ModelError`, and
`HarnessError::from` for session operations and build-time errors. Both classify by one
`ErrorKind` and `is_retryable()`, keep the original message and source chain, and return
the original error through `get_ref()` and `into_inner()`. Only `RateLimited` (`429`),
`Overloaded` (`500`, `502`-`504`, `529`), `Transport`, and `Busy` (including a locked or
saturated store) are retryable; a retryable provider failure also carries its
`Retry-After` in seconds as `retry_after()`, capped at sixty. A spent quota (`402`, or a
`429` with OpenAI's `insufficient_quota` or Kimi's `exceeded_current_quota_error`) is
`QuotaExceeded`, not retryable, and not retried internally; `401`/`403` are `Auth`.
Custom models report the same classes with `ModelError::provider` and
`ModelError::quota_exceeded`. `TurnErrorType` stays the telemetry label.

## Stores

SQLite is the default (`Harness::database`, or `store::SqliteStore` explicitly).
`store::MemoryStore` gives resumable sessions within one process. Custom backends
implement `store::SessionStore` and are injected with `Harness::store`. A store supplies
atomic record operations: `reserve_turn` and `switch_model` read an `AdmissionState` and
record the decision from `TurnAdmission::admit` or `ModelSwitch::admit`, while the
harness owns the admission rules and every lease. `complete_turn` also hands the store
the session's `store::HistoryProjection`, which later loads apply and a model switch
resets. The shared conformance suite is in `tests/support/store_conformance.rs`.

## Observability

`Harness::with_lifecycle_observer` receives turn, model, and tool events that never
contain prompts or output. `lifecycle::LifecycleMetrics` and
`lifecycle::LifecycleTraceObserver` export them to OpenTelemetry. Turn spans nest under
the caller's OpenTelemetry context, including for `.start()`ed turns.
`LifecycleTraceObserver::with_tool_attributes` repeats attributes a host derives from
the turn's context, such as run identity, on its `execute_tool` spans. A failed turn's
`LifecycleEventKind::TurnFailed` carries the `TurnReport` of what it did before failing:
its reported requests, tool calls, and counts. Each fitted request reports the
calibrated capacity it was fitted into (`budget_weight`) and its weight before
(`unreduced_weight`) and after (`estimated_weight`) reduction.

## Where things live

The crate root holds what a typical host needs: `Harness`, `Session`, `TurnOptions`,
`TurnOutcome`, `TurnError`, `SessionError`, `RunError`, `HarnessError`, `ErrorKind`,
`OutputSchema`, `Tool`, `ToolPolicy`, `Repository`, `TurnInput`, `Model`,
`ModelCatalog`, `ModelSelector`, and `Role`. Everything else is grouped by topic:

| Module      | Contents                                                 |
| ----------- | -------------------------------------------------------- |
| `provider`  | Muse, Kimi, Qwen, OpenAI, and OpenAI-compatible clients  |
| `catalog`   | Model specs, roles, capabilities, pricing, and selection |
| `model`     | The `Model` trait, requests, registry, context budgets   |
| `tool`      | Tool arguments, results, and the `FileSystem` seam       |
| `bash`      | Bash configuration, executors, enforcement, records      |
| `turn`      | Turn builders, reports, activity, and settlement errors  |
| `store`     | Session stores and durable write and checkpoint records  |
| `recovery`  | Execution identities and idempotent host request records |
| `lifecycle` | Content-free events and OpenTelemetry observers          |

The API docs on each type carry the detailed contracts. The
[design page](https://agentty.xyz/docs/architecture/ag-harness-design/) covers runtime
and persistence boundaries.
