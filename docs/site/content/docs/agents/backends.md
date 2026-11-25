+++
title = "Agents & Models"
description = "Supported agent backends, available models, and how to configure them."
weight = 1
+++

<a id="backends-introduction"></a> Agentty delegates coding work to external AI agent
CLIs running in session worktrees. Install and authenticate at least one backend.

<!-- more -->

## Supported Backends

<a id="backends-supported-backends"></a> Each backend's CLI must be on your `PATH`;
Agentty fails at startup with an install hint when none is found.

- Codex (`codex`, recommended; supports subscription usage): install the
  [Codex CLI](https://github.com/openai/codex), then run `codex login`.
- Claude (`claude`): install [Claude Code](https://github.com/anthropics/claude-code),
  then run `claude auth login`.
- Antigravity (`agy` 1.5.8 or newer): install the
  [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli), then run
  `agy` and follow its sign-in flow.

All CLI backends run their turns non-interactively inside the session worktree and
accept pasted prompt images. Claude and Codex accept chat sent into a running turn;
other backends read queued chat on their next turn. See
[Active Turns](@/docs/usage/workflow.md#active-turns-and-the-message-queue).

Agentty uses each provider's official non-interactive CLI or app-server surface
(`claude -p --input-format stream-json`, `agy --input-format stream-json`, or
`codex app-server`) after you authenticate with that provider's CLI. It does not
implement OAuth flows, read provider OAuth tokens directly, or call private provider
APIs. You are responsible for choosing an authentication method permitted for your
account, plan, and usage pattern.

### Permissions

- **Chat turns**: Codex and Claude run commands with full access, outside the provider
  sandbox, so browser tests, local services, PTY tools, container runtimes, and tool
  caches work. Commands are therefore not confined to the session worktree; review the
  diff.
- **Inspection work**: Agentty runs reviews, planning, titles, and read-only graph nodes
  without write access. Codex denies filesystem writes and permission escalation but
  allows shell network requests. Claude runs without a shell, using `Read`, `Glob`,
  `Grep`, `WebSearch`, and `WebFetch`. Antigravity denies shell commands and file
  writes.
- Claude turns can always use `WebSearch` and `WebFetch` without a permission prompt,
  and Claude's file-edit tool cannot edit the main checkout. Treat fetched web content
  as untrusted, keep web-backed prompts specific, and review the diff before merging.
- Codex and Claude load the MCP servers configured for their CLI, including plugin
  servers and Claude.ai connectors. MCP tools follow the provider's own approval rules.
- Agentty instructs agents to keep project edits inside the worktree. When permissions
  allow writes, build, test, and recording commands may still use external caches,
  temporary files, and container storage.

## Authentication and Usage

### Codex

<a id="backends-codex-authentication"></a> Codex supports signing in with ChatGPT
through `codex login` for subscription-backed usage, and Agentty uses the supported
`codex app-server` surface. Usage remains subject to the
[OpenAI Terms of Use](https://openai.com/policies/terms-of-use/).

### Claude

<a id="backends-claude-authentication"></a> For Agentty usage through `claude -p`, use
API-key authentication through Claude Console or a supported cloud provider instead of a
Claude Free, Pro, or Max subscription sign-in. Anthropic's
[Claude Code legal and compliance documentation](https://code.claude.com/docs/en/legal-and-compliance)
describes subscription OAuth as intended for Claude Code and native Anthropic
applications, while developer integrations should use API keys or supported cloud
providers.

If Claude turns or utility prompts fail with `authentication_error`,
`Failed to authenticate`, or `OAuth token has expired`, refresh the CLI session and
retry:

```bash
claude auth login
claude auth status
```

### Antigravity

<a id="backends-antigravity-authentication"></a> For Agentty usage through `agy`
headless mode, prefer authentication backed by a Google Cloud project or API key over a
Google Account subscription sign-in. The
[Antigravity terms](https://antigravity.google/terms) do not currently explain how
subscription access applies when third-party tools invoke headless sessions.

Older `agy` versions are excluded from provider selection; if a session hits one, run
`agy update`. Antigravity keeps conversation context between turns and after a restart.

<a id="backends-gemini-cli-sessions"></a> Agentty no longer runs the Gemini CLI.
Sessions and project defaults saved with the Gemini backend now use Antigravity with the
same `gemini-*` models, starting a fresh conversation on their next turn. Install and
authenticate `agy` (see
[Prepare an Agent Backend](@/docs/getting-started/installation.md#prepare-an-agent-backend))
to keep using them.

## Project Instruction Files

<a id="backends-project-instruction-files"></a> Agentty relies on each backend's native
project-instruction discovery: Codex loads `AGENTS.md`, Claude Code loads `CLAUDE.md`,
and Antigravity loads `AGENTS.md` and `GEMINI.md`. Symlink `CLAUDE.md` and `GEMINI.md`
to a canonical `AGENTS.md` to give every backend the same guidance.

## Selecting a Backend

<a id="backends-selecting-a-backend"></a> Use `/model` to choose an installed backend,
then one of its models.

The **Projects** tab shows installed CLI versions, and `updating...` while each CLI's
native updater refreshes it at startup. If an Antigravity executable changes while
Agentty is running, wait for discovery or restart before retrying a turn.

<a id="backends-persistent-defaults"></a> In **Settings**, choose a model and reasoning
level for each project role; models that support
[fast mode](@/docs/agents/backends.md#backends-switching-models) also offer `Normal` or
`Fast` speed. Other models keep `Normal` speed. Unavailable backend defaults fall back
to an installed backend.

<a id="backends-reasoning-level"></a>

| Role   | Used for                   |
| ------ | -------------------------- |
| Smart  | New sessions               |
| Fast   | Titles and commit messages |
| Review | Focused reviews            |

Session overrides take precedence, and changing defaults does not alter existing
sessions. `Default Response Style` starts new sessions as `Concise`, the default,
`Balanced`, or `Detailed`.

Antigravity maps `xhigh` and `max` reasoning to `high`, except `gemini-4-neon` and
`gemini-4-hydrogen`, which use `xhigh`; `gpt-oss-120b` always uses `medium`, and
`gemini-pro-orchestrator-g1-pro` ignores the level. Claude maps both to `max`, which
`claude-opus-5-5` supports. Codex supports a distinct `max` effort.

## Subagent Limits

Agentty limits each Codex or Claude session to two concurrent subagents, excluding the
parent. Change this with the global `Max Subagents` setting (`1`, `2`, `4`, or `8`) and
restart Agentty to apply it.

Codex limits concurrently open child-agent threads. Claude limits ordinary subagent
spawning, though provider exceptions such as resumed subagents can exceed it.
Enforcement depends on the installed CLI. Antigravity subagents are not capped. These
are per-session provider limits, not host-wide CPU or memory limits.

## Available Models

<a id="backends-available-models"></a> Pickers show only the models below. Stored
project defaults and active sessions that reference a superseded model, such as
`gemini-3.1-pro-preview` or `gemini-3.5-flash-lite`, are upgraded to its replacement;
finished sessions keep their historical model.

### Antigravity Models

- `gemini-4-neon` (default): Gemini 4 Neon model with extra-high reasoning effort.
- `gemini-4-argon`: Gemini 4 Argon model.
- `gemini-4-hydrogen`: Gemini 4 Hydrogen model with extra-high reasoning effort.
- `gemini-pro-orchestrator-g1-pro`: Orchestrator (Argon) model that coordinates
  subagents.
- `gemini-3.8-flash`: Fast Gemini model for agentic and multimodal tasks.
- `claude-opus-5-5` and `claude-sonnet-5-5`: Claude models served through Antigravity.
- `gpt-oss-120b`: Open-weight GPT-OSS 120B model served through Antigravity.

### Claude Models

- `claude-fable-5-2` (default): Claude Fable model for creative, narrative-heavy tasks.
- `claude-opus-5-5`: Latest Claude Opus model for complex agentic tasks.
- `claude-sonnet-5-5`: Balanced Claude model for quality and latency.
- `claude-haiku-5-5`: Fast Claude model for lighter tasks.

### Codex Models

- `gpt-6-astra`: Most capable Codex model for the hardest end-to-end work.
- `gpt-6.1-sol` (default): Near-Astra Codex model for complex work at a lower cost.
- `gpt-6-luna`: Efficient Codex model for focused, high-volume tasks.
- `gpt-5.6-terra`: Current Codex model for balanced coding performance.
- `gpt-5.3-codex-spark`: Codex spark model for quick coding iterations.

### Harness Models

Listed under [`ag-harness`](@/docs/agents/backends.md#backends-harness) in `/model`,
alongside local models stored as `openai-compatible/<model>`:

- Muse: `muse-spark-1.3` and the discounted `muse-spark-1.3-contributor`.
- Kimi: `kimi-k3` (latest) and `kimi-k2.6`.
- Qwen: `qwen3.8-max`, `qwen-plus`, `qwen3.8-flash`, and `qwen3.7-flash`.
- ChatGPT plan: `chatgpt/gpt-6.1-sol`, `chatgpt/gpt-6.1-terra`, `chatgpt/gpt-6-astra`,
  `chatgpt/gpt-6-sol`, `chatgpt/gpt-6-luna`, `chatgpt/gpt-5.6-sol`,
  `chatgpt/gpt-5.6-terra`, and `chatgpt/gpt-5.6-luna`.
- OpenAI API: `gpt-6.1-sol`, `gpt-6-astra`, `gpt-6-luna`, and `gpt-5.6-terra`.
- Anthropic API: `claude-sonnet-5-5`, `claude-opus-5-5`, `claude-fable-5-2`, and
  `claude-haiku-5-5`.

Each request fits within an input budget below the model's context window, leaving out
older turns that no longer fit, including after switching to a smaller model.

## Switching Models

<a id="backends-switching-models"></a> Use `/model` to change the current session's
backend and model, `/reasoning` to change reasoning effort, and `/style` to choose
answer length; see [Slash Commands](@/docs/usage/workflow.md#slash-commands). While a
turn runs, model and speed changes wait for
[queued work](@/docs/usage/workflow.md#active-turns-and-the-message-queue).

`/fast` toggles the session between `Normal` and `Fast` speed, which reduces latency at
higher provider cost. It is supported by `claude-opus-5-5` and every Codex model except
`gpt-5.3-codex-spark`; on other models it shows an error and keeps the current model.
Choosing an incompatible model resets speed to `Normal`. Antigravity has no speed
control. See the provider guides for
[Codex fast mode](https://learn.chatgpt.com/docs/agent-configuration/speed) and
[Claude Code fast mode](https://code.claude.com/docs/en/fast-mode).

<a id="backends-switching-default-model"></a> Session changes do not alter project
defaults; change those in [Settings](@/docs/usage/workflow.md#settings-scope).

## Harness Agent

<a id="backends-harness"></a> `/model` lists an **ag-harness** agent after the installed
agent CLIs. It runs Agentty's built-in agent loop instead of an external CLI, against a
free local model server or a paid hosted provider:

| Provider     | Cost | Key imported from                    | Endpoint                              |
| ------------ | ---- | ------------------------------------ | ------------------------------------- |
| Local server | Free | Optional `OPENAI_COMPATIBLE_API_KEY` | Optional `OPENAI_COMPATIBLE_BASE_URL` |
| Muse         | Paid | `MODEL_API_KEY`                      | Optional `MODEL_API_BASE_URL`         |
| Kimi         | Paid | `KIMI_API_KEY`                       | Optional `KIMI_BASE_URL`              |
| Qwen         | Paid | `DASHSCOPE_API_KEY`                  | Required `DASHSCOPE_BASE_URL`         |
| OpenAI       | Paid | `OPENAI_API_KEY`                     | Optional `OPENAI_BASE_URL`            |
| Anthropic    | Paid | `ANTHROPIC_API_KEY`                  | Optional `ANTHROPIC_BASE_URL`         |
| ChatGPT plan | Plan | `ag-harness login chatgpt` sign-in   | Optional `CHATGPT_BASE_URL`           |

Kimi defaults to Moonshot AI's international endpoint, `https://api.moonshot.ai/v1`; set
`KIMI_BASE_URL` to use another, such as `https://api.moonshot.cn/v1`.

<a id="backends-harness-api-keys"></a> Harness API keys live in
`~/.agentty/auth/api_keys.json` (or `$AGENTTY_ROOT/auth/api_keys.json`), readable only
by you. Pressing `Enter` on a hosted `/model` row whose provider has no stored key opens
a popup where you paste the key; Agentty saves it and switches the session to the model.
A key exported in the table's variable is copied into the file the first time Agentty
finds none stored for that provider; after that the file wins and the variable is
ignored. To replace a key, edit its entry in the file, or remove the entry and select
that provider's model in `/model` to paste a new key.

`/model` lists every hosted model, but one runs only once its key, sign-in, or local
server is available: a model without its key asks for it, and a `chatgpt/` model offers
its sign-in. While an agent CLI is installed, new sessions keep starting on the
configured CLI model. Without one, Agentty starts only when a harness model is ready or
you picked a local model earlier, and new sessions start on it; a model that still needs
a key or sign-in is never chosen as the default.

- Inspection turns can read the worktree. Chat turns can also write files and run
  `bash`. Provider keys and endpoints are removed from the `bash` environment.
- <a id="backends-harness-sandbox"></a> `bash` runs in the operating system's sandbox
  where it is available: Seatbelt on macOS, Bubblewrap on Linux. Commands can read the
  disk but write only the session worktree, the session branch, and the session's own
  temporary directory. Credential stores such as `~/.ssh`, `~/.aws`, `~/.config/gh`, the
  keychains, and browser profiles stay hidden, and so do Agentty's database, pasted
  images, harness history, stored API keys, and ChatGPT sign-in. Agentty never reruns a
  blocked command outside the sandbox; it shows a `[Sandbox]` hint when a command may
  have been blocked.
- Commands keep network access, including to services listening on `localhost` such as
  local databases or a Docker TCP socket, but cannot reach Unix sockets: not Docker's,
  and not the SSH agent's, so Git over SSH with agent keys fails inside a session.
  Agentty pushes outside the sandbox itself.
- Environment variables that look like credentials (names containing `TOKEN`, `SECRET`,
  `PASSWORD`, or `CREDENTIAL`, ending in `_KEY`, connection strings such as
  `DATABASE_URL`, and every `AWS_`, `GH_`, `GITHUB_`, and `GITLAB_` variable) never
  reach commands, besides the model providers' keys. The GitHub CLI is therefore
  unauthenticated inside a session.
- Git writes stay limited: commands can commit on the session branch, but cannot move
  the base branch, create branches or tags, use `git stash`, or edit `packed-refs`, so
  `git gc` fails without changing the repository. They cannot write Git configuration,
  hooks, replacement refs, grafts, object alternates, or commit-graph files either, and
  Agentty's own Git ignores replacement refs and grafts. On Linux, commands can still
  move other sessions' branches, since the sandbox there can only make the whole `wt/`
  directory writable. A turn, task, or graph node that moves or deletes the session's
  base branch, or points the worktree's `HEAD` at another branch, fails before Agentty
  commits or publishes anything, with how to restore it, and no turn starts while the
  base does not resolve.
- The temporary directory, `~/.agentty/harness/<session>/tmp/`, holds the Cargo
  (`CARGO_HOME`), npm, Go, pip, and `XDG_CACHE_HOME` caches of that session only, and is
  removed with the session; host tools and other sessions never read it. Commands can
  change anything inside it, but cannot move or remove the directory itself.
- <a id="backends-harness-cargo-cache"></a> Rust projects keep a Cargo download cache in
  `~/.agentty/cache/cargo/`, one per project. When a session's worktree has a
  `Cargo.lock` naming crates.io packages the cache lacks, Agentty fetches them outside
  the sandbox in the background, when the session starts and before each turn, and shows
  a short note in the session. The fetch reads only the lockfile's crates.io entries,
  never the worktree's manifest, so Git dependencies, path dependencies, and other
  registries a session names are never fetched. It also reads none of your Cargo
  configuration or credentials: it runs your default toolchain's `cargo` in a fresh
  directory under `XDG_RUNTIME_DIR` or `TMPDIR`, used only when no other user can write
  above it (a shared `/tmp` does not qualify, so without such a directory Agentty does
  not cache), with its own empty Cargo home, passing on only `PATH`, `HOME`, the
  `rustup` settings, and proxy and certificate variables, and it keeps only archives
  from crates.io itself. If your network reaches crates.io only through a mirror, the
  fetch fails quietly and sessions keep downloading per session. From the next turn,
  Agentty copies the cached archives the session's lockfile names into its own
  `CARGO_HOME`, each checked against the lockfile's checksum, so locked builds download
  only what the cache misses. Sandboxed commands cannot read or write the cache, and
  what one session changes in its own `CARGO_HOME` never reaches another. The cache
  holds the crates.io archives your projects' lockfiles pinned, and Cargo removes those
  no fetch used for three months; delete `~/.agentty/cache/cargo/` to reclaim the space
  at any time. Projects that replace crates.io in their own `.cargo/config.toml`, Git
  dependencies, and other tools' caches are still downloaded per session, and a failed
  fetch only means the session downloads its dependencies itself.
- Your Cargo configuration still applies: Cargo finds `~/.cargo/config.toml` above a
  worktree in the home directory, and otherwise Agentty links your `CARGO_HOME`'s
  configuration into the sandbox before each turn, replacing whatever configuration or
  credential files commands left there. Other settings kept in `~/.cargo` do not reach
  sandboxed commands.
- Commands cannot install toolchains or tools into the home directory: `rustup`
  toolchain installs (including those a `rust-toolchain.toml` triggers), `go install`
  into `~/go/bin`, and Gradle, Maven, or pnpm homes in `~/.gradle`, `~/.m2`, or the pnpm
  store fail. Install them in a terminal outside the session, then run the session's
  commands again.
- Where the sandbox cannot run, commands run without it, and the session shows a
  `[Sandbox Warning]` as soon as the turn starts, naming what is missing and how to fix
  it. It appears once, and again when the reason changes or after the sandbox worked in
  between, which a `[Sandbox]` note confirms. On Linux, install Bubblewrap (for example
  `sudo apt install bubblewrap`), allow unprivileged user namespaces, and run kernel 6.2
  or later; on macOS, start Agentty outside other sandboxes. Restart Agentty after
  upgrading it, since the running process uses its own executable as the sandbox helper.
  Sync conflict assistance and execution-graph task turns in a session warn the same
  way. Merge conflict assistance, commit assistance, and other utility prompts never run
  commands without the sandbox: where it cannot run they fail with the same fix.
- Answers and reasoning stream into the session view as the model writes them, with a
  short "Preparing a tool call" line while tool arguments arrive; the activity footer
  shows tool calls while a turn runs. Streamed text is replaced by the saved answer when
  the turn ends, and only the saved answer is kept; a stopped or failed turn keeps only
  its error notice.
- Commit messages, titles, and focused reviews use the project's default models when
  their agent CLI is installed, otherwise the session's harness model.
- History is stored per session under `~/.agentty/harness/` and removed with the
  session. Pasted images are not supported yet; such prompts fail before reaching the
  model.
- A content-free report of each completed turn (token usage, context fitting,
  corrections, and tool activity) is kept in the `harness_turn_report` database table,
  including after the session is deleted.
- A session can switch models between turns. A model from another provider sees earlier
  reasoning as short notes of at most 1 KiB each, and switching back restores it.
- <a id="backends-harness-compaction"></a> Long sessions compact automatically: once
  earlier turns nearly fill the model's context window, the harness summarizes them
  before the turn and later turns carry the summary instead. The session view then shows
  a `[Context]` notice naming the model that wrote the summary, or a `[Context Warning]`
  when summarizing failed and the oldest turns were dropped instead; these notices are
  never sent to models. The summary runs on the
  [Default Fast Model](@/docs/agents/backends.md#backends-harness-fast-model) when it is
  a ready harness model with structured output, such as `muse-spark-1.3-contributor`,
  `gpt-6-luna`, `claude-haiku-5-5`, or a `chatgpt/` model, and otherwise on the
  session's own model; Qwen, Kimi, and local models declare no structured output. Its
  tokens count toward the session's usage, and its report entry is priced at the
  summarizing model's prices. Task agents and graph nodes do not compact.

<a id="backends-harness-fast-model"></a> **Settings → Default Fast Model** also lists
every ready harness model with its price class, such as `harness/qwen3.8-flash · cheap`,
to run commit messages and titles for every session type, and to summarize long harness
sessions when it has structured output, which this Qwen example lacks. Agentty never
picks a harness model for this itself; while the chosen model is not ready, the Fast
default falls back as if unset.

### Choosing a Model

<a id="backends-harness-model"></a> In any session, `/model` lists `ag-harness` after
the installed agent CLIs. Choosing it lists every harness model: local models first,
marked `[local]` with their server and context window, then hosted models marked
`[paid]` with the same descriptions as the other agents' models, with OpenAI and Claude
models last:

```text
[local] qwen3:latest              Ollama · runs 32k ctx
[local] gemma:latest              Ollama · no tool calls
[paid] muse-spark-1.3-contributor Cheap Muse Spark tier; Meta may train on its prompts…
[paid] kimi-k3                    Latest Kimi model served through the native harness.
[paid] gpt-6.1-sol                Near-Astra Codex model for complex work at a lower cost.
[paid] claude-opus-5-5            Latest Claude Opus model for complex agentic tasks.
```

Switching a session between `ag-harness` and a CLI agent keeps the conversation: the
next turn replays the session transcript to the new agent.

Price classes follow each provider's list price per million output tokens: `cheap` is at
most $2.50, `premium` at least $15, and `standard` in between. The cheap models,
`muse-spark-1.3-contributor`, `qwen-plus`, `qwen3.8-flash`, `qwen3.7-flash`, and
`gpt-6-luna`, and `claude-haiku-5-5`, suit simple work such as titles and commit
messages. Meta may train on prompts and completions sent to
`muse-spark-1.3-contributor`.

Pressing `Enter` on a row that needs an API key or sign-in opens a popup to provide it.
On a row that cannot run a turn otherwise, such as a model without tool calls or a
provider without its base URL, it keeps the current model and shows what is missing,
such as `` `qwen3.8-flash` needs `DASHSCOPE_BASE_URL` ``. The list refreshes each time
you open it, so newly pulled models and started servers appear without a restart.

The harness model you pick never changes the project default model. It becomes the model
new sessions start on when no backend CLI is installed. Until you pick one, those
sessions prefer a ready local model, then the first ready paid model in list order. An
imported API key, such as `OPENAI_API_KEY` or `ANTHROPIC_API_KEY` set for other tools,
therefore never displaces a ready local model or a provider listed before it; when
nothing else is ready, its first model, `gpt-6.1-sol` or `claude-sonnet-5-5`, becomes
the default. A remembered local model stays the default while its server is stopped; a
remembered paid model whose key is gone gives way to the next ready model.

### OpenAI Models

<a id="backends-harness-openai"></a> The harness runs `gpt-6.1-sol`, `gpt-6-astra`,
`gpt-6-luna`, and `gpt-5.6-terra` through OpenAI's Responses API with a paid OpenAI
Platform API key: choose a `gpt-*` model under `ag-harness` in `/model` and paste the
key, or export `OPENAI_API_KEY` before the first launch to import it.

- Usage is billed to the key's organization. To use a ChatGPT subscription instead, pick
  a `chatgpt/` model
  ([ChatGPT Plan Models](@/docs/agents/backends.md#backends-harness-chatgpt)) or run a
  **Codex** session.
- `OPENAI_BASE_URL` (default `https://api.openai.com/v1`) selects another Responses API
  endpoint.
- Each request resends the session history, billed at OpenAI's cached-input rate where
  cached. Prompts over 272,000 input tokens cost twice the input rate, which long
  sessions can reach.

### ChatGPT Plan Models

<a id="backends-harness-chatgpt"></a> Models with a `chatgpt/` prefix, such as
`chatgpt/gpt-6.1-sol`, use the same Responses API billed to your ChatGPT plan instead of
API credits. Sign in once; the harness stores and refreshes the sign-in in
`~/.agentty/auth/chatgpt.json` (or `$AGENTTY_ROOT/auth/chatgpt.json`, or
`CHATGPT_AUTH_FILE`):

```sh
ag-harness login chatgpt
agentty   # then choose a chatgpt/ model under ag-harness in /model
```

- Usage counts against your plan's limits.
- Pressing `Enter` on such a row, or sending a prompt on a signed-out `chatgpt/` model,
  opens a sign-in popup that opens your browser and then switches the session to the
  model.
- A `chatgpt/` model always uses the plan and a bare `gpt-*` model always uses the
  stored OpenAI API key; neither falls back to the other.

### Claude Models

<a id="backends-harness-claude"></a> The harness runs `claude-sonnet-5-5`,
`claude-opus-5-5`, `claude-fable-5-2`, and `claude-haiku-5-5` through Anthropic's
Messages API with a Claude Console API key: choose a `claude-*` model under `ag-harness`
in `/model` and paste the key, or export `ANTHROPIC_API_KEY` before the first launch to
import it.

- Usage is billed to the key's Console organization at API rates, never to a Claude
  subscription.
- `ag-harness` and the **claude** agent share these model names and descriptions but not
  their backend: `ag-harness` rows, badged `[paid]`, always use the stored Anthropic API
  key, and `claude` rows always use the `claude` CLI; neither falls back to the other.
  Settings and the status bar show them as `claude/<model>` and `harness/<model>`, and
  harness rows in Settings add their price class, such as
  `harness/claude-haiku-5-5 · cheap`.
- `ANTHROPIC_BASE_URL` (default `https://api.anthropic.com/v1`) selects another Messages
  API endpoint. It also accepts the origin alone, such as `https://api.anthropic.com`,
  as Anthropic's SDKs and Claude Code read it.
- Each request resends the session history, billed at Anthropic's cache-read rate where
  cached. A `claude-haiku-5-5` request whose prompt exceeds 100,000 tokens, cached
  tokens included, costs five times its listed rates, which long sessions can reach;
  reported costs include it.
- When fitting a long session to the context window rewrites earlier turns, Anthropic
  may refuse the reasoning recorded after them: always for `claude-fable-5-2`, and for
  the 5.5 models only on API keys of organizations created on or after August 31, 2026.
  The harness then has the API drop that reasoning, re-sending a refused 5.5 request
  once, and the model continues without it; older organizations keep it on the 5.5
  models.
- `ANTHROPIC_` variables and `CLAUDE_CODE_OAUTH_TOKEN` are removed from the `bash`
  environment.

### Local and Self-Hosted Models

<a id="backends-harness-local"></a> Agentty lists the models served by an
OpenAI-compatible Chat Completions endpoint, by default Ollama at
`http://127.0.0.1:11434/v1`, so a running Ollama needs no configuration. Other servers,
such as llama.cpp `llama-server`, LM Studio, or vLLM, are listed through their models
endpoint.

```sh
OLLAMA_CONTEXT_LENGTH=32768 ollama serve   # or raise the Ollama app's context length
agentty
```

| Variable                              | Default                     | Meaning                                    |
| ------------------------------------- | --------------------------- | ------------------------------------------ |
| `OPENAI_COMPATIBLE_BASE_URL`          | `http://127.0.0.1:11434/v1` | Endpoint including `/v1`; Ollama's default |
| `OPENAI_COMPATIBLE_API_KEY`           | None                        | Bearer token, such as vLLM's `--api-key`   |
| `OPENAI_COMPATIBLE_MODEL`             | None                        | Model to list when the server lists none   |
| `OPENAI_COMPATIBLE_CONTEXT_WINDOW`    | `32768`                     | Tokens the server runs the model with      |
| `OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS` | A quarter of the window     | Tokens reserved for each response, ≤ 8192  |

- The model must support tool calls. Typical base URLs are `http://127.0.0.1:8080/v1`
  for `llama-server`, `http://127.0.0.1:1234/v1` for LM Studio, and
  `http://127.0.0.1:8000/v1` for vLLM.
- Ollama loads models with a 4,096-token window unless `OLLAMA_CONTEXT_LENGTH` or the
  app setting raises it, and silently drops the start of longer requests. Agentty reads
  the window an Ollama model runs with before each turn, fits requests to it, and stops
  the turn below 16,384 tokens, asking you to raise the window. The picker shows a
  loaded model's running window, as in `runs 4k ctx, needs 16k`, or otherwise its
  maximum, as in `up to 40k ctx`.
- For servers that report no running window, set `OPENAI_COMPATIBLE_CONTEXT_WINDOW` to
  the window the server actually uses. Agentty budgets input for these models with a 50%
  margin.
- The reasoning level is not sent; the model uses its server default.
