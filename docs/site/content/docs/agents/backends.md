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
- Antigravity (`agy` 1.1.18 or newer): install the
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

- **Auto Edit**: Codex and Claude run commands with full access, outside the provider
  sandbox, so browser tests, local services, PTY tools, container runtimes, and tool
  caches work. Commands are therefore not confined to the session worktree; review the
  diff, and use `Read Only` for inspection-only work.
- **Read Only**: Codex denies filesystem writes and permission escalation but allows
  shell network requests. Claude runs without a shell, using `Read`, `Glob`, `Grep`,
  `WebSearch`, and `WebFetch`. This also applies to read-only graph nodes.
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
sessions. `Default Response Style` starts new sessions as `Concise`, `Balanced`, or
`Detailed`.

Antigravity maps `xhigh` and `max` reasoning to `high`, except `gemini-4-hydrogen`,
which uses `xhigh`; `gpt-oss-120b` always uses `medium`, and
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

- `gemini-4-argon` (default): Gemini 4 Argon model.
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

Available only in [Harness sessions](@/docs/agents/backends.md#backends-harness),
alongside local models stored as `openai-compatible/<model>`:

- Muse: `muse-spark-1.3` and the discounted `muse-spark-1.3-contributor`.
- Kimi: `kimi-k3` (latest) and `kimi-k2.6`.
- Qwen: `qwen3.8-max`, `qwen-plus`, `qwen3.8-flash`, and `qwen3.7-flash`.
- ChatGPT plan: `chatgpt/gpt-6.1-sol`, `chatgpt/gpt-6.1-terra`, `chatgpt/gpt-6-astra`,
  `chatgpt/gpt-6-sol`, `chatgpt/gpt-6-luna`, `chatgpt/gpt-5.6-sol`,
  `chatgpt/gpt-5.6-terra`, and `chatgpt/gpt-5.6-luna`.
- OpenAI API: `gpt-6.1-sol`, `gpt-6-astra`, `gpt-6-luna`, and `gpt-5.6-terra`.
- Anthropic API: `claude-opus-5-5`, `claude-fable-5-2`, `claude-sonnet-5-5`, and
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

## Harness Sessions (Preview)

<a id="backends-harness"></a> Start Agentty with `agentty --experimental-harness` to add
a **Harness** session type that runs Agentty's built-in agent loop instead of an
external CLI, against a free local model server or a paid hosted provider:

| Provider     | Cost | Key                                  | Endpoint                              |
| ------------ | ---- | ------------------------------------ | ------------------------------------- |
| Local server | Free | Optional `OPENAI_COMPATIBLE_API_KEY` | Optional `OPENAI_COMPATIBLE_BASE_URL` |
| Muse         | Paid | `MODEL_API_KEY`                      | Optional `MODEL_API_BASE_URL`         |
| Kimi         | Paid | `KIMI_API_KEY`                       | Optional `KIMI_BASE_URL`              |
| Qwen         | Paid | `DASHSCOPE_API_KEY`                  | Required `DASHSCOPE_BASE_URL`         |
| OpenAI       | Paid | `OPENAI_API_KEY`                     | Optional `OPENAI_BASE_URL`            |
| ChatGPT plan | Plan | `ag-harness login chatgpt` sign-in   | Optional `CHATGPT_BASE_URL`           |
| Anthropic    | Paid | `ANTHROPIC_API_KEY`                  | Optional `ANTHROPIC_BASE_URL`         |

Kimi defaults to Moonshot AI's international endpoint, `https://api.moonshot.ai/v1`; set
`KIMI_BASE_URL` to use another, such as `https://api.moonshot.cn/v1`.

Without the flag the session type is hidden; with it but no runnable model, it is shown
disabled. With a runnable model, Agentty starts even when no backend CLI is installed,
disabling the other session types.

- `Read Only` turns can read the worktree. `Auto Edit` turns can also write files and
  run unsandboxed `bash`, which can reach the network and paths outside the worktree.
  Provider keys and endpoints are removed from the `bash` environment.
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

<a id="backends-harness-fast-model"></a> With the flag, **Settings → Default Fast
Model** also lists every ready harness model with its price class, such as
`harness/qwen3.8-flash · cheap`, to run commit messages and titles for every session
type. Agentty never picks a harness model for this itself; while the chosen model is not
ready, or without the flag, the Fast default falls back as if unset.

### Choosing a Model

<a id="backends-harness-model"></a> In a Harness session, `/model` lists only harness
models: local models first, marked `[local]`, then hosted models marked `[paid]` with
their price class, with OpenAI and Anthropic models last:

```text
[local] qwen3:latest              free · Ollama · runs 32k ctx
[local] gemma:latest              free · Ollama · no tool calls
[paid] muse-spark-1.3-contributor cheap · Muse · trains on prompts · 1024k ctx
[paid] kimi-k3                    premium · Kimi · needs KIMI_API_KEY
[paid] gpt-6.1-sol                standard · OpenAI · needs OPENAI_API_KEY
```

Price classes follow each provider's list price per million output tokens: `cheap` is at
most $2.50, `premium` at least $15, and `standard` in between. The cheap models,
`muse-spark-1.3-contributor`, `qwen-plus`, `qwen3.8-flash`, `qwen3.7-flash`,
`gpt-6-luna`, and `claude-haiku-5-5`, suit simple work such as titles and commit
messages. Meta may train on prompts and completions sent to
`muse-spark-1.3-contributor`.

Rows that cannot run a turn, such as a model without tool calls or a provider without
its key, say what is missing and cannot be selected. The list refreshes each time you
open it, so newly pulled models and started servers appear without a restart.

The model you pick becomes the default for new Harness sessions; it never changes the
project default for other session types. Until you pick one, new sessions prefer a ready
local model, then the first ready paid model in list order, so an exported API key, such
as `OPENAI_API_KEY` or `ANTHROPIC_API_KEY` set for other tools, never makes a paid
provider the default. A remembered local model stays the default while its server is
stopped; a remembered paid model whose key is gone gives way to the next ready model.

### OpenAI Models

<a id="backends-harness-openai"></a> The harness runs `gpt-6.1-sol`, `gpt-6-astra`,
`gpt-6-luna`, and `gpt-5.6-terra` through OpenAI's Responses API with a paid OpenAI
Platform API key:

```sh
export OPENAI_API_KEY=sk-...
agentty --experimental-harness   # then choose a gpt-* model in the Harness /model list
```

- Usage is billed to the key's organization. To use a ChatGPT subscription instead, pick
  a `chatgpt/` model
  ([ChatGPT Plan Models](@/docs/agents/backends.md#backends-harness-chatgpt)) or run a
  **Codex** session.
- `OPENAI_BASE_URL` (default `https://api.openai.com/v1`) selects another Responses API
  endpoint.
- Each request resends the session history, billed at OpenAI's cached-input rate where
  cached. Prompts over 272,000 input tokens cost twice the input rate, which long
  sessions can reach.

### Claude Models in the Harness

<a id="backends-harness-claude"></a> The harness runs `claude-opus-5-5`,
`claude-fable-5-2`, `claude-sonnet-5-5`, and `claude-haiku-5-5` through Anthropic's
Messages API with a Claude Console API key:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
agentty --experimental-harness   # then choose a claude-* model in the Harness /model list
```

- Usage is billed to the key's Anthropic organization at API rates, not to a Claude
  subscription; to use a subscription, run a **Claude** session.
- A Claude session keeps running on Claude Code and a Harness session on the harness;
  choosing the same model in one never moves the other.
- `ANTHROPIC_BASE_URL` (default `https://api.anthropic.com`) selects another endpoint.
  As in Anthropic's SDKs, it names the origin without `/v1`.
- `ANTHROPIC_` variables and `CLAUDE_CODE_OAUTH_TOKEN` are removed from the `bash`
  environment.

### ChatGPT Plan Models

<a id="backends-harness-chatgpt"></a> Models with a `chatgpt/` prefix, such as
`chatgpt/gpt-6.1-sol`, use the same Responses API billed to your ChatGPT plan instead of
API credits. Sign in once; the harness stores and refreshes the sign-in in
`~/.agentty/auth/chatgpt.json` (or `$AGENTTY_ROOT/auth/chatgpt.json`, or
`CHATGPT_AUTH_FILE`):

```sh
ag-harness login chatgpt
agentty --experimental-harness   # then choose a chatgpt/ model in the Harness /model list
```

- The rows show `plan` instead of a price class, and `needs ag-harness login chatgpt`
  until you sign in or after OpenAI revokes the sign-in. Usage counts against your
  plan's limits.
- Pressing `Enter` on such a row, or sending a prompt on a signed-out `chatgpt/` model,
  opens a sign-in popup that opens your browser and then switches the session to the
  model.
- A `chatgpt/` model always uses the plan and a bare `gpt-*` model always uses
  `OPENAI_API_KEY`; neither falls back to the other.

### Local and Self-Hosted Models

<a id="backends-harness-local"></a> Agentty lists the models served by an
OpenAI-compatible Chat Completions endpoint, by default Ollama at
`http://127.0.0.1:11434/v1`, so a running Ollama needs no configuration. Other servers,
such as llama.cpp `llama-server`, LM Studio, or vLLM, are listed through their models
endpoint.

```sh
OLLAMA_CONTEXT_LENGTH=32768 ollama serve   # or raise the Ollama app's context length
agentty --experimental-harness
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
