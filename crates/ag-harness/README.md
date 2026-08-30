# `ag-harness`

Run a model with bounded repository tools and validated structured output.

## Library

```rust
use ag_harness::{Harness, MUSE_SPARK_1_3, Muse, Tool};

let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_3)?)
    .repository(&repository_root)
    .allow(Tool::Read)
    .allow(Tool::Write);

let output = harness.run(prompt, output_schema).await?;
```

Provide `MODEL_API_KEY`, a repository root, explicitly allowed tools, a prompt, and an
`OutputSchema`. Expect schema-validated JSON or a typed error.

Tools are denied by default. Applications enable the closed built-in `Tool::Read` and
`Tool::Write` capabilities; they cannot register arbitrary model tools. The `read` tool
offers five bounded actions: `file` reads worktree text, `list` discovers repository
paths, `search` finds literal text, `diff` compares against `main`, and `show` reads a
file from `main` or `HEAD`. The fixed base keeps the v0 application API to
`.repository(root).allow(Tool::Read)` and prevents the model from selecting a revision.

`write` applies one bounded unified diff to one text file. File reads and writes use the
injectable `FileSystem`; repository inspection runs fixed read-only Git operations
behind a private command boundary. That boundary clears inherited Git configuration,
uses an absolute Git executable outside the configured root, disables configured
filesystem monitors, verifies that root against Git's canonical worktree, and drains
command streams while retaining complete bounded records. When a provider returns
multiple tool calls in one response, the harness validates the complete batch, executes
it in provider order, and records one assistant message followed by every tool result.

Use `Harness::chat()` for sequential in-memory turns and sanitized activity reports.
Chat history retains complete recent turns within a 256 KiB payload budget; use
`Harness::max_history_bytes()` to override it.

Use `ModelWithMetadata::complete_with_metadata()` for normalized completion metadata.

Use `ModelProvider` and `ModelConfiguration` to discover built-in providers and
construct a provider client from its standard environment variables. The catalog owns
known model identifiers, credential variables, endpoint variables, and provider defaults
so applications do not need provider-specific construction branches.

For ChatGPT-subscription-backed Codex, install the `codex` executable and authenticate
it with ChatGPT before constructing the model directly:

```rust
use ag_harness::{Codex, CodexConfig, Harness};

let model_name = std::env::var("CODEX_MODEL")?;
let model = Codex::new(CodexConfig::new(model_name))?;
let output = Harness::new(model).run(prompt, output_schema).await?;
```

This experimental v0 reads ChatGPT OAuth credentials from `CODEX_HOME/auth.json` (or
`~/.codex/auth.json`) and sends a streaming Responses request directly to the ChatGPT
Codex endpoint. It rejects API-key authentication rather than silently incurring API
charges. The endpoint is unofficial and may change; use it only where the account,
workspace, plan, and applicable OpenAI terms permit. The adapter does not refresh OAuth
tokens, so reauthenticate with Codex after an expired-token response. Set `CODEX_MODEL`
only to a model verified for the account and the `ag-harness` originator; API model
availability does not establish routing through this unofficial endpoint. Harness tool
definitions are not supported by this v0 adapter.

Attach `with_lifecycle_observer()` to receive ordered metadata-only lifecycle events.
After installing an OpenTelemetry meter provider, attach `LifecycleMetrics::new()` to
project standard agent and client-side tool metrics. Use `LifecycleObserverSet` to send
the same stream to multiple observers, such as metrics, traces, and a host-owned
journal.

`LifecycleTraceObserver` projects that stream to OpenTelemetry GenAI spans. Install a
tracer provider in the application before starting an operation, then attach the
observer to a client or harness:

```rust
use ag_harness::LifecycleTraceObserver;

let harness = Harness::new(model)
    .with_lifecycle_observer(LifecycleTraceObserver::new());
```

## Telemetry

When the application installs an OpenTelemetry meter provider, `ModelClient` records
request duration and provider-reported input and output tokens. `LifecycleMetrics`
projects end-to-end harness-turn duration, per-turn model and tool call counts, and
executed-tool duration. The trace observer emits correlated `invoke_agent`,
`chat {model}`, and `execute_tool {tool}` spans. It propagates each model and tool
context while the corresponding asynchronous operation is polled, so provider,
HTTP-client, and tool instrumentation becomes a child of the projected span. Missing
usage is not estimated, sensitive content is excluded, and the application owns export
and shutdown. The emitted signals follow the pinned OpenTelemetry GenAI
semantic-convention contract documented in the architecture guide.

The companion `ag-harness-cli` package provides an interactive command-line chat powered
by this library. The manual real-model compatibility benchmark and its latest recorded
results live in `tests/benchmark/README.md`.
