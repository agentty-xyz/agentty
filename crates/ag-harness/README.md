# `ag-harness`

Run a model with bounded repository tools and validated structured output.

```rust
use ag_harness::{Harness, MUSE_SPARK_1_2, Muse, Tool};

let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_2)?)
    .repository(repository_root)
    .allow(Tool::Read)
    .allow(Tool::Write);

let output = harness.run(prompt, output_schema).await?;
```

Provide `MODEL_API_KEY`, a repository root, explicitly allowed tools, a prompt, and an
`OutputSchema`. Expect schema-validated JSON or a typed error.

Tools are denied by default and repository tools require an explicit root. `read`
returns bounded file content; `write` applies one bounded unified diff to one text file.
Both use the injected `FileSystem` boundary without shell commands, and the model may
continue calling allowed tools until it returns schema-valid JSON.

Use `ModelWithMetadata::complete_with_metadata()` for normalized completion metadata.

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
