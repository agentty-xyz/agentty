# `ag-harness`

`ag-harness` runs structured LLM turns with deny-by-default repository permissions and
durable sessions. Every turn validates against a caller-supplied output schema, and the
selected session store — not the provider — is the source of truth for conversation
state.

## One-shot turn

```rust
use ag_harness::{Harness, Muse, MUSE_SPARK_1_3};

let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_3)?);
let result = harness.run_once("Summarize Cargo.toml", output_schema).await?;
```

`run_once` executes one turn without storage. Use it for stateless utility calls.

## Durable sessions

```rust
use ag_harness::{Harness, Muse, MUSE_SPARK_1_3, Repository, Tool};

let repository = Repository::new(".", git_executable)?;
let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_3)?)
    .database("harness.db")
    .repository(repository)
    .allow(Tool::Read);

let mut session = harness
    .session("review-42", output_schema.clone())
    .system_prompt("Keep the review concise.")
    .create()
    .await?;
let result = session.send("Review the current changes").await?;

// Later, in the same or another process:
let mut session = harness.resume("review-42").await?;
let result = session.send("Now focus on error handling").await?;
```

Completed turns are replayed on resume; failed and interrupted turns stay visible in the
store but never re-enter model context. Sessions run concurrently, with one active turn
per session. The library never picks a database location — configure it with
`Harness::database()` (the companion CLI defaults to `~/.ag-harness/db/harness.db`).

## Choosing an entry point

| Need                                        | Use                                           |
| ------------------------------------------- | --------------------------------------------- |
| One stateless turn                          | `run_once`                                    |
| Multi-turn conversation                     | `session(...).create()` / `resume` + `send`   |
| Per-turn schema, permissions, or comparison | `send_with_options` / `run_once_with_options` |
| Cancellation and shutdown observation       | `send_controlled` / `run_once_controlled`     |
| Idempotent retries by host-assigned ID      | `submit` + `recover`                          |

Explicit `TurnOptions` replace harness defaults for that turn; they are never merged.
Controlled turns return a `TurnControl` that can `cancel()` and then observe
`settled()`, `effects_settled()`, and `commands_settled()` independently of the caller's
future. `submit` fingerprints the effective request so a matching retry returns the
recorded outcome without executing a provider or tool again.

## Tools and permissions

Tools are denied by default and enabled through `ToolPolicy`:

- `Tool::Read` — file, list, search, and `show(head)` inspection. Comparisons (`diff`,
  `show(base)`) additionally require a host-pinned `ComparisonBase` in `TurnOptions`.
- `Tool::Write` — applies one bounded unified diff.
- `Tool::Bash` — additionally requires an explicit `TurnOptions::with_bash(BashConfig)`
  per turn.

Both repository tools need a `Repository` with a trusted Git executable outside the
containing worktree.

## Sandboxed Bash

`BashConfig::new` selects the native sandbox: install the matching `ag-harness-sandbox`
binary from this crate outside the execution workspace and supply its absolute path plus
a trusted Bash. Linux uses Bubblewrap, Landlock (Linux 6.2+), and seccomp; macOS uses
Seatbelt with best-effort process-group cleanup. `BashConfig::for_executor` selects a
host-supplied `BashExecutor` instead, including the shipped
`UnsandboxedExecutor::without_isolation` for hosts already inside a container or VM.
Selection is always explicit — no fallback, no environment-based choice.

Workspace access defaults to read-only. Grant writes with `with_write`, external reads
with `with_read`, environment values with `with_environment`, and host-detail exposure
with the mandatory `with_host_information`. Git metadata stays read-only and networking
is denied; unsupported policy fails closed on the native executor. Command intents
persist before spawning; await `commands_settled()` (and `retry_commands()` after
failures) so unresolved commands never silently block new turns. See `CommandOutcome`
and `Session::commands()` for recorded results.

## Models

Built-in Muse, Kimi, and Qwen clients construct from the environment, or implement the
object-safe `Model` trait to inject a provider. `ModelRegistry` selects models by stable
host keys with declared `ModelCapabilities`:

```rust
use ag_harness::{ExecutionIdentity, Harness, ModelCapabilities, ModelRegistry, Muse, MUSE_SPARK_1_3};

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

Capabilities are host declarations, not detection. A declared `ContextBudget` enables
model-aware context projection: requests keep the most recent whole turns that fit, and
mandatory content that cannot fit fails with a typed error before any provider call.
Registered sessions resume only with the same registration key and revision; revise the
revision whenever configuration, credentials scope, or injected behavior changes, and
never embed secrets. `session.switch_model(&models, "other-model")` selects another
registration for an idle session; `session.compact()` publishes a schema-validated
summary checkpoint that projection replays ahead of recent turns.

## Session stores

SQLite is the default (`Harness::database` or an explicit `SqliteStore`). `MemoryStore`
provides resumable in-process sessions without restart durability. Custom backends
implement the public `SessionStore` contract and are injected with `Harness::store`; the
shared conformance suite lives in `tests/support/store_conformance.rs`.

## Input

Every entry point accepts `impl Into<TurnInput>`, so plain strings keep working. Ordered
text and image content uses explicit `InputBlock`s with validated PNG/JPEG bytes. Image
support is opt-in per model and checked before any provider request.

## Observability

Attach `Harness::with_lifecycle_observer()` for content-free turn, model, and tool
events. `LifecycleMetrics` and `LifecycleTraceObserver` project the stream to
OpenTelemetry without storing prompts or tool output in telemetry.

## Going deeper

The Rust API docs on each type carry the detailed contracts — settlement and recovery
semantics, store conformance, Bash grant validation, and image limits. The
[design page](https://agentty.xyz/docs/architecture/ag-harness-design/) covers runtime
and persistence boundaries.
