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
use std::num::NonZeroU64;

use ag_harness::model::ContextBudget;
use ag_harness::provider::{MUSE_SPARK_1_3, Muse};
use ag_harness::{Harness, OutputSchema};
use serde_json::json;

let schema = OutputSchema::new(json!({
    "type": "object",
    "properties": { "summary": { "type": "string" } },
    "required": ["summary"],
}))?;
let budget = ContextBudget::new(NonZeroU64::new(128_000).ok_or("budget")?)
    .with_reserved_output(16_384)?;
let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_3)?, budget);

let outcome = harness.run_once("Summarize Cargo.toml", schema).await?;
println!("{}", outcome.output()["summary"]);
```

Every harness needs a `ContextBudget`: the approximate request weight the model's
context window allows, in roughly token-sized units. `run_once` keeps no history. For a
conversation, use a session.

## Sessions

```rust
use ag_harness::{Harness, Repository, Tool};

let harness = Harness::new(model, budget)
    .database("harness.db")
    .repository(Repository::new(".", git_executable)?)
    .allow(Tool::Read);

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

Every request replays the most recent finished turns that fit the context budget; the
provider never holds conversation state. A turn too large to replay whole replays with
its tool results, reasoning, and `write` patches replaced by short notices, or else as
its input and final answer; the history byte limit applies the same forms when loading,
and a turn larger than eight times that limit loads only as its input and final answer.
Each model response and tool result is stored as soon as it finishes. A failed or
interrupted turn replays what it recorded plus a note naming why it stopped and any
writes or commands whose outcome is unknown; when it does not fit, it replays with its
tool traffic replaced by notices, or else as only its input and note. Sessions run
concurrently, with one active turn per session. The library never picks a database
location for you.

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
- **Turns have no tool-call limit.** Cancellation and the `ContextBudget` bound a turn.
  Once tool traffic grows a request past the budget, the request is sent as a reduced
  copy without an extra model call. First, older tool results that a later call
  superseded (a re-read of the same target, an applied write to the file read, or a
  rerun of the same command) become notices naming that call. Then any tool result above
  25% of the input budget is cut to a head and tail excerpt, and the latest round's
  largest results are cut to one common size while together they exceed 50%; an excerpt
  stays the same in later requests. Then the other older tool results become notices
  (`read` notices name the target and may be re-read; `bash`/`write` notices keep the
  outcome and say not to repeat), then older reasoning and `write` patches are cleared,
  then the oldest replayed turns are dropped, then the current turn's oldest tool rounds
  are dropped whole behind one note naming the writes they applied, the commands they
  ran, and the reads to redo only if still needed, down to 70% of the budget so later
  requests share a stable prefix. Only as a last resort are the latest tool round's
  results cut further and its `write` patches replaced by their recorded outcome. Stored
  messages are never changed; `ModelRequestActivity::context` reports each reduction. A
  request that still cannot fit fails with `TurnError::ContextBudgetExceeded`.
- **Provider usage calibrates the estimate.** Each response's reported input tokens
  calibrate later requests: a line through the first reported request, sloped by the
  steepest token growth since and lifted over every report, so providers' fixed
  per-request cost does not inflate large requests. Projection, admission, and fitting
  keep calibrated requests within 95% of the input budget, and use 90% of the estimate
  until a provider reports usage. The calibration lives in memory, is shared by a
  harness and its sessions, and resets when a session switches model or the estimator is
  replaced. `ModelRequestActivity::estimated_weight` and `calibrated_weight` report what
  each request was fitted as.
- **Identical reads are answered from the earlier result.** A `read` whose arguments
  match an earlier read of the same turn returns a short note naming that call instead
  of running again, while the last request still carried the earlier result whole, no
  `write` or `bash` call ran since, and the result is larger than the note. The turn
  report lists it as `ToolActivity::ReadRepeated`. `write` and `bash` always run.
- **Invalid final output gets two corrections.** A final answer that is not valid JSON
  or violates the schema is re-requested with short, request-only feedback up to twice
  per turn; `TurnReport::output_corrections` counts these retries, and a third rejection
  fails the turn.
- **Malformed tool calls get error results.** A call whose arguments its tool cannot
  decode, such as invalid JSON or a misspelled field, is journaled with its raw
  arguments and answered with an error result naming the problem and the tool's fields,
  while the other calls of the same response still run.
  `TurnReport::malformed_tool_calls` counts them and each is listed as
  `ToolActivity::MalformedArguments`. A third consecutive response made only of
  malformed calls fails the turn; any decodable call resets the count.

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

### Sandboxed Bash

`BashConfig::new` selects the native sandbox: Bubblewrap, Landlock, and seccomp on
Linux; Seatbelt on macOS. Install the matching `ag-harness-sandbox` binary outside the
workspace and pass its absolute path plus a trusted Bash. `BashConfig::for_executor`
selects your own `BashExecutor` instead, such as
`UnsandboxedExecutor::without_isolation` for hosts already inside a container or VM.
There is never a silent fallback.

The workspace is read-only unless you grant `with_write`. External reads (`with_read`),
environment values (`with_environment`), and host details (`with_host_information`) are
explicit too. Networking is always denied. Long output keeps each stream's start and end
within the capture budget, with the omitted byte counts on the `CommandOutcome`. Command
intents are recorded before spawning; await `settled()` (and `retry_settlement()` after
failures) so unresolved commands never block new turns silently.

## Models

`provider::Muse::from_env` builds the Muse client from environment variables. For any
built-in provider selected at runtime, use `provider::ModelConfiguration`:

```rust
use ag_harness::provider::{KIMI_K3, ModelConfiguration, ModelProvider};

let kimi = ModelConfiguration::new(ModelProvider::Kimi, KIMI_K3)
    .client_from_environment(|name| std::env::var(name))?;
```

Built-in clients call `ag-router` for provider transport and schema validation. Hosts
can also use `ag-router` directly for structured chat calls without a harness. Implement
the `model::Model` trait to bring any other provider into `ag-harness`.

`model::ModelRegistry` selects models by stable host keys with declared capabilities:

```rust
use ag_harness::model::{ModelCapabilities, ModelRegistry};
use ag_harness::recovery::ExecutionIdentity;

let mut models = ModelRegistry::new();
models.register(
    ExecutionIdentity::new("review-model", "config-v1")?,
    Muse::from_env(MUSE_SPARK_1_3)?,
    ModelCapabilities {
        image_input: true,
        tool_calls: true,
        ..ModelCapabilities::new(budget)
    },
)?;
let harness = Harness::from_registry(&models, "review-model")?;
```

- Capabilities are host declarations, not detection. The required `ContextBudget` keeps
  the most recent turns that fit and fails before any provider call when mandatory
  content cannot fit. `TurnReport::history` says what was replayed or dropped.
- `session.switch_model(&models, key)` changes the model of an idle session.
  `session.compact()` stores a schema-validated summary of the oldest uncovered turns
  that fit, which later turns replay first; call it again to cover the rest.
- Bump the registration revision whenever configuration or credentials scope changes.
  Never put secrets in it.

Built-in clients accept JSON wrapped in Markdown fences or prose when the embedded
object parses; anything else fails with `ModelError::InvalidJson`. Requests time out
after three minutes, and a `429` `Retry-After` is honored up to sixty seconds. A
provider rejection of input beyond the model's window reports
`ModelErrorType::ContextOverflow` (`ModelError::is_context_overflow`); custom models
report it with `ModelError::context_overflow`.

## Stores

SQLite is the default (`Harness::database`, or `store::SqliteStore` explicitly).
`store::MemoryStore` gives resumable sessions within one process. Custom backends
implement `store::SessionStore` and are injected with `Harness::store`. A store supplies
atomic record operations: `reserve_turn` and `switch_model` read an `AdmissionState` and
record the decision from `TurnAdmission::admit` or `ModelSwitch::admit`, while the
harness owns the admission rules and every lease. The shared conformance suite is in
`tests/support/store_conformance.rs`.

## Observability

`Harness::with_lifecycle_observer` receives turn, model, and tool events that never
contain prompts or output. `lifecycle::LifecycleMetrics` and
`lifecycle::LifecycleTraceObserver` export them to OpenTelemetry. Turn spans nest under
the caller's OpenTelemetry context, including for `.start()`ed turns.

## Where things live

The crate root holds what a typical host needs: `Harness`, `Session`, `TurnOptions`,
`TurnOutcome`, `TurnError`, `SessionError`, `OutputSchema`, `Tool`, `ToolPolicy`,
`Repository`, `TurnInput`, and `Model`. Everything else is grouped by topic:

| Module      | Contents                                                 |
| ----------- | -------------------------------------------------------- |
| `provider`  | Built-in Muse, Kimi, and Qwen clients                    |
| `model`     | The `Model` trait, requests, registry, context budgets   |
| `tool`      | Tool arguments, results, and the `FileSystem` seam       |
| `bash`      | Sandboxed Bash configuration, executors, command records |
| `turn`      | Turn builders, reports, activity, and settlement errors  |
| `store`     | Session stores and durable write and checkpoint records  |
| `recovery`  | Execution identities and idempotent host request records |
| `lifecycle` | Content-free events and OpenTelemetry observers          |

The API docs on each type carry the detailed contracts. The
[design page](https://agentty.xyz/docs/architecture/ag-harness-design/) covers runtime
and persistence boundaries.
