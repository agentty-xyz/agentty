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
use ag_harness::provider::{MUSE_SPARK_1_3, Muse};
use ag_harness::{Harness, OutputSchema};
use serde_json::json;

let schema = OutputSchema::new(json!({
    "type": "object",
    "properties": { "summary": { "type": "string" } },
    "required": ["summary"],
}))?;
let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_3)?);

let outcome = harness.run_once("Summarize Cargo.toml", schema).await?;
println!("{}", outcome.output()["summary"]);
```

`run_once` keeps no history. For a conversation, use a session.

## Sessions

```rust
use ag_harness::{Harness, Repository, Tool};

let harness = Harness::new(model)
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

Resuming replays completed turns only; failed and interrupted turns stay visible in the
store but never re-enter model context. Sessions run concurrently, with one active turn
per session. The library never picks a database location for you.

## Turns

`run_once` and `send` run a turn with the defaults. When a turn needs more, call `turn`
and chain what you need before awaiting it:

```rust
let outcome = session
    .turn("Review against main")
    .options(TurnOptions::new(schema, ToolPolicy::default().allow(Tool::Read), limits))
    .host_id("request-7")
    .await?;
```

| Need                                    | Chain                                       |
| --------------------------------------- | ------------------------------------------- |
| Different schema, tools, or limits      | `.options(TurnOptions::new(...))`           |
| Safe retries under a host request ID    | `.host_id(id)`, later `session.recover(id)` |
| Cancellation and settlement observation | `.start()` instead of `.await`              |
| A one-shot turn with explicit options   | `harness.turn(input, options)`              |

- **Options replace defaults for that turn; they never merge.** Session defaults stay
  unchanged.
- **Host IDs make a turn idempotent.** A retry with the same ID and the same effective
  request returns the recorded outcome without calling the model or tools again. This
  requires an `ExecutionIdentity` on the harness (or a registered model).
- **`start()` returns a `ControlledTurn`.** Its `control()` can `cancel()` the turn and
  then wait on `settled()`, `effects_settled()`, and `commands_settled()` independently
  of the caller's future.

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

### Sandboxed Bash

`BashConfig::new` selects the native sandbox: Bubblewrap, Landlock, and seccomp on
Linux; Seatbelt on macOS. Install the matching `ag-harness-sandbox` binary outside the
workspace and pass its absolute path plus a trusted Bash. `BashConfig::for_executor`
selects your own `BashExecutor` instead, such as
`UnsandboxedExecutor::without_isolation` for hosts already inside a container or VM.
There is never a silent fallback.

The workspace is read-only unless you grant `with_write`. External reads (`with_read`),
environment values (`with_environment`), and host details (`with_host_information`) are
explicit too. Networking is always denied. Command intents are recorded before spawning;
await `commands_settled()` (and `retry_commands()` after failures) so unresolved
commands never block new turns silently.

## Models

`provider::Muse::from_env` builds the Muse client from environment variables. For any
built-in provider selected at runtime, use `provider::ModelConfiguration`:

```rust
use ag_harness::provider::{KIMI_K2_6, ModelConfiguration, ModelProvider};

let kimi = ModelConfiguration::new(ModelProvider::Kimi, KIMI_K2_6)
    .client_from_environment(|name| std::env::var(name))?;
```

Implement the `model::Model` trait to bring any other provider.

`model::ModelRegistry` selects models by stable host keys with declared capabilities:

```rust
use ag_harness::model::{ModelCapabilities, ModelRegistry};
use ag_harness::recovery::ExecutionIdentity;

let mut models = ModelRegistry::new();
models.register(
    ExecutionIdentity::new("review-model", "config-v1")?,
    Muse::from_env(MUSE_SPARK_1_3)?,
    ModelCapabilities {
        context_budget: None,
        image_input: true,
        native_continuation: false,
        tool_calls: true,
    },
)?;
let harness = Harness::from_registry(&models, "review-model")?;
```

- Capabilities are host declarations, not detection. A declared `ContextBudget` keeps
  the most recent whole turns that fit and fails before any provider call when mandatory
  content cannot fit. `TurnReport::history` says what was replayed or dropped.
- `session.switch_model(&models, key)` changes the model of an idle session.
  `session.compact()` stores a schema-validated summary that later turns replay first.
- Bump the registration revision whenever configuration or credentials scope changes.
  Never put secrets in it.

Built-in clients accept JSON wrapped in Markdown fences or prose when the embedded
object parses; anything else fails with `ModelError::InvalidJson`. Requests time out
after three minutes, and a `429` `Retry-After` is honored up to sixty seconds.

## Stores

SQLite is the default (`Harness::database`, or `store::SqliteStore` explicitly).
`store::MemoryStore` gives resumable sessions within one process. Custom backends
implement `store::SessionStore` and are injected with `Harness::store`; the shared
conformance suite is in `tests/support/store_conformance.rs`.

## Observability

`Harness::with_lifecycle_observer` receives turn, model, and tool events that never
contain prompts or output. `lifecycle::LifecycleMetrics` and
`lifecycle::LifecycleTraceObserver` export them to OpenTelemetry.

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
