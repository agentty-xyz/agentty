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

# Chat with an OpenAI model (paid API key)
OPENAI_API_KEY=sk-... cargo run --locked -p ag-harness-cli -- \
  run gpt-6.1-sol --provider openai

# Chat with a Claude model (paid Anthropic API key)
ANTHROPIC_API_KEY=sk-ant-... cargo run --locked -p ag-harness-cli -- \
  run claude-opus-5-5 --provider anthropic

# Chat on your ChatGPT Plus or Pro plan instead of API credits (sign in once)
cargo run --locked -p ag-harness-cli -- login chatgpt
cargo run --locked -p ag-harness-cli -- run gpt-6.1-sol --provider chatgpt

# Chat with a local Ollama model (no key; server running with a 40,960-token window)
OPENAI_COMPATIBLE_CONTEXT_WINDOW=40960 cargo run --locked -p ag-harness-cli -- \
  run qwen3:latest --provider openai-compatible
```

`--provider openai-compatible` works with any OpenAI-compatible server: set
`OPENAI_COMPATIBLE_BASE_URL` (or `--base-url`) for llama.cpp, LM Studio, or vLLM, and
`OPENAI_COMPATIBLE_API_KEY` when the server requires one.

`login chatgpt` opens "Sign in with ChatGPT" in your browser, registers Agentty with
OpenAI on first use, and stores the refreshing sign-in at `~/.agentty/auth/chatgpt.json`
(`$AGENTTY_ROOT/auth/chatgpt.json`, or `CHATGPT_AUTH_FILE`), readable only by you.
`--provider chatgpt` then bills requests to that plan's usage limits; it never falls
back to an API key. When the plan limit is reached, requests fail until it resets.

## Chat commands

Type `/` in the chat to list commands:

- `/model` lists the cataloged models of every provider with credentials, numbered in ID
  order, with their list prices and the roles (`smart`, `cheap`, `fast`, `vision`) whose
  first choice they are.
- `/model <MODEL>` switches later turns of the session to a list number, a role's first
  choice, a `provider/model` pair, a known model ID (of the current provider when it has
  one), or another model ID from the current provider. Role names win over model IDs;
  use `provider/model` for a model named like a role. Completed history is kept, and
  resuming the session uses the new model.
- `/usage` shows the session's summed tokens and cost.
- `/history` shows the session's turns: each prompt and answer.
- `/help` shows the commands.

Switching needs the target provider's credentials. `--base-url` applies only to the
provider it was given for; other providers read their base URL environment variable.
Reasoning from another provider's model is replayed as short notes. A model whose
`provider/model` exceeds 256 bytes can start a session but cannot be a switch target.

After each answer, `usage:` shows the turn's tokens and its cost from the model's list
prices. These report `cost unavailable`: turns after a `/model` switch, sessions created
before costs were reported, and models without prices, such as ChatGPT-plan, local, and
uncataloged models. If a provider's credentials are set but unusable, such as an
unreadable ChatGPT sign-in, the chat warns and continues without the catalog: `/model`
lists no models and no turn reports a cost.

## Serve a protocol (unstable)

`serve` lets non-Rust programs drive sessions over standard input and output with
JSON-RPC 2.0, one JSON message per line. The protocol is **unstable**: methods and
message shapes may change in any release.

```sh
cargo run --locked -p ag-harness-cli -- serve
```

```json
{"jsonrpc":"2.0","id":1,"method":"session/create","params":{"model":{"role":"smart"},"output":{"type":"json","schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}}}}
{"jsonrpc":"2.0","id":2,"method":"run/start","params":{"session":"<id from 1>","input":"Hello"}}
```

- Methods: `session/create`, `session/resume`, `run/start`, `run/cancel`,
  `session/transcript`, `session/usage`, and `shutdown`.
- Models are selected by catalog role (`{"role": "smart"}`), catalog ID, or
  `{"id": "provider/model"}`, with credentials from the environment as for `run`.
  Sessions live in the server's memory unless `store` names a SQLite file;
  `session/resume` reads the model from that file when `model` is omitted. Of the global
  options only `--reasoning-effort`, and `--git-executable` with `--read-dir`, apply;
  the others are rejected.
- `run/start` answers with a run ID at once. The run's events follow as `run/event`
  notifications and end with a `completed` event that carries its usage, cost, and any
  failure. `run/cancel` succeeds for any run the server started, even a finished one.
- After `completed`, `session/transcript` shows the terminal status of a run that
  emitted `started`, unless the turn's cleanup failed; the schema's `completed`
  description has the details.
- Errors are JSON-RPC errors; harness failures carry `data.kind` and `data.retryable`.
  Malformed lines are answered with an error and the server keeps running. A message
  without `id` is executed but never answered.
- Without `--read-dir <DIR>`, tools are denied. With it, runs may `read` that
  repository, and with `--allow-write` also `write` to it. Each write first asks the
  client: the run's `approval_requested` event is followed by an `approval/decide`
  server request, which the client answers with the same `id` and a `result` of
  `{"decision": "allow"}`, `{"decision": "allow_with", "arguments": {...}}`, or
  `{"decision": "deny", "feedback": "..."}`. An error answer, or a `result` that is not
  a decision (also reported with a `-32602` error), denies the write. A run whose answer
  does not arrive within `--approval-timeout-ms` (five minutes by default) fails with
  `approval_timeout`, and a late answer is ignored.
- `shutdown` or closing standard input cancels runs in flight, writes their last events,
  and exits.

`serve --print-schema` prints the JSON Schema of every message, committed as
[`schema/serve_protocol.json`](schema/serve_protocol.json); its `x-method-results` names
each method's result, and `x-server-request-results` the answer to each server request.

## Defaults

- Repository writes are disabled unless `--allow-write` is set.
- Sessions are stored in `~/.ag-harness/db/harness.db`. Set `AG_HARNESS_ROOT`, or pass
  `--database <FILE>`, to choose another location. If `HOME` is unavailable, one of
  those explicit locations is required.
- The first valid Git in `PATH` is used; `--git-executable <FILE>` overrides it.
- Muse is the default provider. Run `cargo run --locked -p ag-harness-cli -- run --help`
  for Kimi, Qwen, OpenAI, OpenAI-compatible, model, and credential options.
- Chats default to low model reasoning to reduce latency; pass
  `--reasoning-effort <LEVEL>` to select deeper reasoning.
- Uncataloged models and Qwen models other than the flash ones are assumed to have a
  128k-token context window; cataloged OpenAI, Muse, Kimi, and Qwen flash models use
  their documented windows; OpenAI-compatible models use
  `OPENAI_COMPATIBLE_CONTEXT_WINDOW` (default 32,768) and
  `OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS` (default a quarter of the window, at most
  8,192). Each request replays the most recent turns that fit, and a turn fails once its
  own tool traffic no longer fits.

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
chat. A `/v1/traces` URL also exports content-free request duration and token-usage
histograms to the sibling `/v1/metrics` URL.
