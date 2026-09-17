# `ag-harness`

`ag-harness` runs structured LLM turns with explicit repository permissions and durable
SQLite sessions, process-local memory sessions, or host-provided session stores.

## Durable sessions

```rust
use ag_harness::{
    ComparisonBase, Harness, Muse, MUSE_SPARK_1_3, Repository, Tool, ToolPolicy,
    TurnLimits, TurnOptions,
};

let repository = Repository::new(".", git_executable)?;
let options = TurnOptions::new(
    output_schema.clone(),
    ToolPolicy::default().allow(Tool::Read),
    TurnLimits::default(),
)
.with_comparison_base(ComparisonBase::resolve(&repository, "HEAD").await?);
let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_3)?)
    .database("harness.db")
    .repository(repository)
    .allow(Tool::Read);

let mut session = harness
    .session("review-42", output_schema.clone())
    .system_prompt("Keep the review concise.")
    .create()
    .await?;

let result = session
    .send_with_options("Review the current changes", options.clone())
    .await?;
println!("{}", result.output());
```

Resume a stored session and supply the desired options for the next turn:

```rust
let mut session = harness.resume("review-42").await?;
let result = session.send_with_options("Now focus on error handling", options).await?;
```

The selected store is the source of truth. A completed turn retains the user prompt,
assistant messages, tool calls, and tool results. Failed and interrupted turns remain
visible in the database but are not replayed. Different sessions can run concurrently;
one session accepts only one active turn at a time.

Write intents and outcomes remain available through `session.writes().await?` after
failure, reopen, or history eviction. These records describe past operations, not the
current filesystem. `run_once` does not create a durable write journal.

The library does not choose a database location. Configure it once with
`Harness::database()`. The companion CLI defaults to `~/.ag-harness/db/harness.db`;
override that with `AG_HARNESS_ROOT` or `--database`.

## Cancellation and settlement

Use `Session::send_controlled` or the storage-free `Harness::run_once_controlled` with
explicit `TurnOptions`. The returned `ControlledTurn` starts when polled. Retain its
`TurnControl` independently to cancel or observe settlement after dropping the future:

```rust
let turn = session.send_controlled("Review the changes", options);
let control = turn.control();
tokio::pin!(turn);
let result = tokio::select! {
    result = &mut turn => Some(result),
    () = shutdown_signal => {
        control.cancel();
        None
    }
};
control.settled().await?;
control.effects_settled().await?;
```

Cancellation stops the waiter promptly; an in-progress terminal commit can still
succeed. Keep the Tokio runtime running until `settled()` acknowledges execution and
persistence cleanup. Failed cleanup returns a bounded `SettlementError` and retains
local admission; after fixing storage, `retry_settlement()` retries only that turn's
owner, then `settled()` observes the result. Repeated cancellation and stale controls
cannot stop a successor turn. Acquisition abandoned before acknowledgment never starts
model or tool execution.

`effects_settled()` separately waits until the turn can start no more writes and all
managed replacements have acknowledged completion and attempted journal recording.
Started replacements and outcome recording survive cancellation and caller-future drop,
including ordinary turns. Local session admission stays protected through both
persistence cleanup and managed effects. Existing write intents can settle after lease
expiry; cancellation never replays a replacement.

An `EffectSettlementError` with `is_unresolved()` means a worker stopped without
acknowledging filesystem completion; local admission stays blocked until process exit,
even if controls are dropped. A journal-recording error reports known filesystem
completion but leaves the durable outcome pending. `retry_settlement()` only retries
owner cleanup, not writes or outcome recording. Inspect durable history and write
records after cancellation. Neither boundary provides rollback, distributed workspace
fencing, or proof that remote providers and unrelated processes have stopped.

## Custom session stores

Implement `SessionStore` and pass a shared instance to `Harness::store`. SQLite remains
available through `Harness::database` or an explicitly opened `SqliteStore`:

```rust
use std::{path::Path, sync::Arc};
use ag_harness::{Harness, SessionStore, SqliteStore};

let store: Arc<dyn SessionStore> = Arc::new(SqliteStore::open(Path::new("harness.db")).await?);
let harness = Harness::new(model).store(store);
```

Use the built-in `MemoryStore` for resumable sessions within one process:

```rust
use std::sync::Arc;
use ag_harness::{Harness, MemoryStore};

let store = MemoryStore::new();
let harness = Harness::new(model).store(Arc::new(store.clone()));
let mut session = harness.session("scratch", output_schema).create().await?;
let result = session.send("Summarize the task").await?;
```

Clones share state and identity; separately constructed stores are independent. Memory
storage retains turn history and write journals only while its shared state lives and
provides no restart durability. Its history budget bounds replay, not total retained
memory. Filesystem changes made by tools outlive the memory journal.

The latest `store` or `database` selection applies to new handles; existing builders and
sessions retain their captured store. One-shot execution never accesses storage.

Independent handles for one backing store must share a `StoreIdentity`. The harness
coordinates local admission; each backend must atomically enforce ownership and leases
across processes. Build an `AcquiredTurn` before committing its reservation, retain it
through acknowledgment, then activate it. Forward the supplied store handle unchanged so
decorators and admission remain attached to renewal, journals, and cleanup. Failed
cleanup retains admission until owner-scoped recovery succeeds. Keep the Tokio runtime
driven until cleanup settles; this does not establish completion of filesystem effects.

Use `StoredTurnOptions` for historical encoding and continuation compatibility, and
`ModelMessage::retained_bytes` for bounded whole-turn history. `SessionError::Store`
retains backend errors without requiring SQL types. The external test implementation and
shared conformance cases are in `tests/support/store_conformance.rs`.

## Host turn recovery

Configure a stable execution identity before submitting host-assigned turn IDs:

```rust
use ag_harness::ExecutionIdentity;

let harness = harness.execution_identity(ExecutionIdentity::new("my-model-config", "v1")?);
let mut session = harness.resume("task").await?;
let result = session.submit("request-42", "Summarize the task", options).await?;
let recorded = session.recover("request-42").await?;
```

The identity is a host assertion covering model configuration and injected execution
behavior, including custom filesystems. Revise it when endpoints, credential scopes,
model configuration, or injected implementations change; never embed secrets. Input,
schema, permissions, limits, comparison identity, repository scope, system prompt,
reasoning settings, and the stored history budget are fingerprinted separately from
`StoredTurnOptions` compatibility. Conversation history and remote continuation IDs do
not change a retry's identity.

Host IDs are unique within a session. Matching completed retries return the original
`TurnOutcome`, including its recorded activity report. Active retries return
`SessionError::HostTurnInProgress`; failed or interrupted retries return
`SessionError::HostTurnStopped`. Both contain the recorded state and known writes. A
changed effective request returns `SessionError::HostTurnConflict`. Retries never
execute a provider or tool, including when a terminal acknowledgment was lost. SQLite
retains these records across reopen and history projection; memory storage retains them
only for its lifetime. Calls without host IDs remain supported.

Use `submit_controlled` for a retained cancellation control. After cancellation settles,
`recover` reports the canonical outcome, which may be completed if cancellation raced
its commit. Pending write intents remain unknown. A deliberate new attempt needs a new
ID; neither a new ID nor persistence settlement proves earlier effects stopped or
provides exactly-once external effects.

External `SessionStore` implementations must implement atomic `begin_request`,
`complete_request`, and `load_request`. Duplicate classification precedes busy
detection; terminal output and messages commit together. Decorators forward the
reservation's original store handle. Host-turn activity duration is the engine duration
recorded before the terminal commit, so recovery returns the identical report.

## One turn

Use `run_once` when no resumable history is needed:

```rust
let result = harness.run_once("Summarize Cargo.toml", output_schema).await?;
```

## Permissions and models

Tools are denied by default. `Tool::Read` enables file, list, search, and `show(head)`.
Comparisons (`diff` and `show(base)`) also require a host-selected `ComparisonBase` in
`TurnOptions`; no base is chosen implicitly. The base stays pinned while worktree and
`HEAD` reads remain live. The companion CLI requires `--comparison-base <REV>` for
comparisons.

`Tool::Write` applies one bounded unified diff. Either tool requires a `Repository` with
a trusted Git executable outside the containing worktree.

External providers implement the single `Model` trait and return `ModelCompletion`. They
receive the complete ordered history in `ModelRequest::messages()`. A provider may also
return an opaque continuation identifier; if native resume is unavailable, the harness
retries once using the stored history and retains any replacement continuation returned
by that replay.

Attach `Harness::with_lifecycle_observer()` for content-free turn, model, and tool
events. The rejected resume and replay are separate model attempts in lifecycle events
and `TurnOutcome::report()`. Host-ID submissions begin execution observation only after
acquiring a new turn; recorded retries and recovery lookups emit no execution events.
