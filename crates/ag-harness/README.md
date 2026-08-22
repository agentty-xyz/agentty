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

## Telemetry

When the application installs an OpenTelemetry meter provider, `ModelClient` records
request duration and provider-reported input and output tokens. `LifecycleMetrics`
projects end-to-end harness-turn duration, per-turn model and tool call counts, and
executed-tool duration. Missing usage is not estimated, sensitive content is excluded,
and the application owns export and shutdown. The emitted instruments follow the pinned
OpenTelemetry GenAI semantic-convention contract documented in the architecture guide.
