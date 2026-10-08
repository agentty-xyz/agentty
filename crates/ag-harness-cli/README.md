# `ag-harness-cli`

Chat with models through a repository-aware, durable terminal harness.

## Get started

```sh
export MODEL_API_KEY="your-key"
cargo run --locked -p ag-harness-cli -- run muse-spark-1.3
```

This starts an interactive chat with read-only access to the current directory.

## Common commands

```sh
# Start a named session
cargo run --locked -p ag-harness-cli -- run muse-spark-1.3 --session review-42

# Resume it later
cargo run --locked -p ag-harness-cli -- resume review-42

# Allow repository writes
cargo run --locked -p ag-harness-cli -- run muse-spark-1.3 --allow-write

# Chat about another repository
cargo run --locked -p ag-harness-cli -- \
  run muse-spark-1.3 --read-dir /path/to/repository
```

## Chat commands

Type `/` in the chat to list commands:

- `/model` lists the known models, numbered, with the current model.
- `/model <MODEL>` switches later turns of the session to a list number, a
  `provider/model` pair, a known model ID, or another model ID from the current
  provider. Completed history is kept, and resuming the session uses the new model.
- `/help` shows the commands.

Switching needs the target provider's credentials. `--base-url` applies only to the
provider it was given for; other providers read their base URL environment variable.
Sessions whose history contains provider reasoning cannot switch models. A model whose
`provider/model` exceeds 256 bytes can start a session but cannot be a switch target.

## Defaults

- Repository writes are disabled unless `--allow-write` is set.
- Sessions are stored in `~/.ag-harness/db/harness.db`. Set `AG_HARNESS_ROOT`, or pass
  `--database <FILE>`, to choose another location. If `HOME` is unavailable, one of
  those explicit locations is required.
- The first valid Git in `PATH` is used; `--git-executable <FILE>` overrides it.
- Muse is the default provider. Run `cargo run --locked -p ag-harness-cli -- run --help`
  for Kimi, Qwen, model, and credential options.
- Chats default to low model reasoning to reduce latency; pass
  `--reasoning-effort <LEVEL>` to select deeper reasoning.
- Every model is assumed to have a 128k-token context window. Each request replays the
  most recent turns that fit, and a turn fails once its own tool traffic no longer fits.

## Tracing

Tracing is disabled unless `--otlp-endpoint` supplies a complete OTLP HTTP/protobuf
traces URL:

```sh
cargo run --locked -p ag-harness-cli -- \
  --otlp-endpoint http://localhost:4318/v1/traces run muse-spark-1.3
```

Each turn exports an `invoke_agent` trace with `chat <model>` and `execute_tool <tool>`
child spans. Spans carry timings, model and tool identities, finish reasons, and token
usage; they never contain prompts, model output, or tool content. Authentication headers
can come from `OTEL_EXPORTER_OTLP_TRACES_HEADERS` or `OTEL_EXPORTER_OTLP_HEADERS`. Spans
are flushed when the chat exits; export failures print a warning without failing the
chat.
