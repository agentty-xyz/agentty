+++
title = "ag-harness Design"
description = "Design for the planned ag-harness runtime."
weight = 6
+++

# `ag-harness` - a light LLM harness

`ag-harness` is the base layer between an application and an LLM. It is Rust-native,
app-facing, and lightweight. It provides the essentials: an agent loop, three core
tools, session management, and a typed event stream, leaving product decisions to the
application.

```mermaid
flowchart LR
    T["TUI"] --> H["ag-harness"]
    C["CLI"] --> H
    W["HTTP"] --> H
    H --> M["Muse"]
    H --> K["Kimi"]
    H --> Q["Qwen"]
    H --> X["Codex"]
    X --> A["ChatGPT endpoint"]
```

## Core features

- **Three built-in tools** - `read`, `write`, and `bash`.
- **Structured edits** - `write` applies git-style patches and emits targeted diffs.
- **Permission policy** - every tool call passes an explicit policy check.
- **Persisted sessions** - session state is saved and can be resumed.
- **Typed events** - applications receive model, tool, diff, completion, and failure
  events.

## Architecture

### Crates

```text
agentty/
└── crates/
    ├── ag-harness            # facade
    ├── ag-harness-cli        # interactive CLI
    ├── ag-harness-protocol   # operations and events
    └── ag-harness-core       # loop, tools, sessions, storage
```

The crates separate the public API, protocol types, runtime behavior, and interactive
CLI. A provider-neutral model contract keeps Muse, Kimi, Qwen, and future adapters
behind the same structured-output and tool-calling boundary.

### Codex subscription backend

Codex is a separate `ModelClient` backend because it uses ChatGPT OAuth and the ChatGPT
Codex Responses endpoint rather than an API-key endpoint. Applications construct it
through `Codex` and `CodexConfig`, outside the API-key-oriented `ModelProvider` catalog.
The experimental backend reads `CODEX_HOME/auth.json`, falling back to
`~/.codex/auth.json`, and accepts only `chatgpt` auth. It ignores a compatibility API
key stored alongside OAuth tokens but rejects `api_key` mode. It sends the access token
and resolved account ID to the pinned `https://chatgpt.com/backend-api/codex/responses`
URL with redirects disabled and both credential headers marked sensitive. The endpoint
is not configurable, preventing token disclosure to another host. ID-token claims supply
missing account IDs and required FedRAMP routing. Authentication opens and inspects one
nonblocking file handle on Tokio's blocking pool and rejects malformed tokens,
non-regular files, and files larger than 64 KiB before sending a request. Each client
binds to its first account ID, accepting refreshed tokens only for that account.

Requests are stateless and streaming, translate system messages into Responses
instructions, and require strict object-schema output. Incremental decoding enforces
request, idle, wire, event, and decoded-content limits and terminates on
`response.completed`. The backend does not refresh OAuth tokens and rejects harness tool
definitions. It is unofficial and must only be used where the account, workspace, plan,
and applicable OpenAI terms permit.

## Session management

- One host process manages multiple independent sessions.
- Active sessions run concurrently as async tasks.
- Each session processes its prompts in sequence.
- Session state is saved on disk and loaded when resumed.
- The application coordinates work across sessions.
- Stopping the host ends active turns while saved sessions remain resumable.

```mermaid
flowchart TB
    H["Harness process"] --> S["Session management"]
    S --> A["Session A"]
    S --> B["Session B"]
    S --> C["Session C"]
    A --> AT["Async task"]
    B --> BT["Async task"]
    C --> CT["Async task"]
```

## Memory management

- Active session state stays in memory.
- Resumable session state is persisted to disk.
- Idle sessions reload from disk when resumed.

## Agent loop

A turn continues until the model responds without requesting a tool:

```mermaid
flowchart TD
    A["User prompt"] --> B["Assemble context"]
    B --> C["Call model"]
    C --> D{"Tool requested?"}
    D -- Yes --> E["Check policy and run tool"]
    E --> B
    D -- No --> F["Turn complete"]
```

### One tool call

```mermaid
sequenceDiagram
    participant M as Model
    participant H as Harness
    participant A as App
    M->>H: Request write patch
    H->>H: Check policy
    H->>H: Apply patch
    H-->>A: Emit diff event
    H->>M: Return tool result
```

## Editing

- **Write** - the model supplies a git-style patch and the harness applies it.
- **Diff** - applied changes are streamed as structured file-diff events.
- **Output caps** - oversized tool output is truncated with a marker.
- **Focused reads** - `read` supports bounded inspection and precise re-reads.

## Context

- **Project discovery** - finds the project root and applicable `AGENTS.md` guidance.
- **Repository exploration** - the model inspects the project through bounded tools.
- **Base prompt** - one minimal system prompt establishes the harness contract.

## Structured output

- The caller supplies the expected JSON shape as a provider-neutral schema.
- Each adapter uses the strongest structured-output mechanism its provider supports.
- The harness parses and validates terminal output against the caller's schema.
- Provider-specific request fields stay inside the adapter.
- Every model adapter implements the same structured-output and tool-calling semantics.

Unsupported configurations return an explicit error instead of silently falling back to
unstructured text.

## Telemetry

- **Metrics** - record model and tool duration, token usage, and outcomes.
- **Labels** - identify the provider and model without application-specific dimensions.
- **Ownership** - the harness records observations; the host configures export.
- **Lifecycle** - applications can observe calls, tools, completion, and failure.

```mermaid
flowchart LR
    C["Model call"] --> M["Runtime metrics"]
    M --> O["OTLP collector"]
    O --> P["Prometheus"]
    P --> G["Grafana"]
```

## Permissions

All tools are denied by default. A session policy explicitly enables tools and, for
`bash`, the commands it may run:

```rust
Policy {
    read: Allow,
    write: Deny,
    bash: AllowCommands(["cargo test", "git status", "rg *"]),
}
```

## Model tiers

- **Model variety** - choose fast or frontier models for the task.
- **Batch/Flex processing** - run latency-tolerant background work at lower cost.
- **Priority processing** - favor response time for latency-sensitive turns.
- **Application choice** - the application selects the model and processing tier.

## Library API

- **Harness** - creates and resumes sessions.
- **Session** - holds the model, policy, working directory, and state.
- **Turn** - runs one prompt and streams typed events.
- **Model** - provides the provider-neutral completion boundary.
- **App** - configures sessions, coordinates work, and renders events.

## Differences from existing harnesses

- **User-facing products** (Claude Code, OpenCode, Aider) format output for humans;
  `ag-harness` emits events for applications.
- **Minimal harnesses** ([Pi](https://pi.dev/)) are TypeScript-first and omit an
  enforced permission layer; `ag-harness` is Rust-native and policy-driven.
- **Vendor SDKs** are provider-specific; `ag-harness` is model-neutral.
- **Rust model API crates** such as `rig` abstract model calls; `ag-harness` adds the
  loop, sessions, tools, permissions, and events.
- **Heavy harnesses** bundle orchestration; `ag-harness` leaves it to the application.

## Next iterations

1. **Complete the tool set.** Add policy-constrained `bash` execution alongside `read`
   and `write`.
1. **Persisted sessions.** Add resumable state and the typed event stream around the
   existing model and tool loop.

## Roadmap

1. **v1 - library.** Integrate the unified `ag-harness` crate into Agentty.
1. **Service wrapper.** Add `ag-harness-service` and `ag-harness-client` for local and
   remote applications.
