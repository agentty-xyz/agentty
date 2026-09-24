+++
title = "Agents & Models"
description = "Supported agent backends, available models, and how to configure them."
weight = 1
+++

<a id="backends-introduction"></a> Agentty delegates coding work to external AI agent
CLIs running in session worktrees. Install and authenticate at least one backend.

<!-- more -->

## Supported Backends

<a id="backends-supported-backends"></a> Agentty supports four agent backends. Each
requires its respective CLI to be installed and available on your `PATH`.

- Codex (`codex`, recommended; supports subscription usage): install the
  [Codex CLI](https://github.com/openai/codex), then run `codex login`.
- Claude (`claude`): install [Claude Code](https://github.com/anthropics/claude-code),
  then run `claude auth login`.
- Antigravity (`agy` 1.1.18 or newer): install the
  [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli), then run
  `agy` and follow its sign-in flow. Agentty excludes older versions from provider
  selection and reports `agy update` as the recovery step if a session encounters an
  outdated executable.
- Gemini (`gemini`): install the
  [Gemini CLI](https://github.com/google-gemini/gemini-cli) and authenticate with an API
  key or Vertex AI.

All backends accept pasted local prompt images from the Agentty composer (`Ctrl+V`,
`Ctrl+Shift+V`, or `Alt+V` in prompt mode) and run their turns non-interactively inside
the session worktree.

Codex `Auto Edit` turns run with full command access so browser tests, local services,
and other tools that cannot run inside the provider sandbox remain available. This also
means Codex commands are not confined to the session worktree; review the session diff
and use `Read Only` for inspection-only work.

Claude turns also allow Claude Code's `WebSearch` and `WebFetch` tools, so prompts that
need current external information can use the web without an interactive permission
grant. Claude `Auto Edit` retains Claude Code's unsandboxed command fallback for tools
that cannot run inside its sandbox.

Treat fetched web content as untrusted context. Claude still has edit-capable tools
during the turn, so keep web-backed prompts specific and review the session diff before
merging.

Agentty requires at least one supported backend CLI on `PATH` at startup and fails with
an install hint when none is found.

Agentty uses each provider's official non-interactive CLI or app-server surface
(`claude -p`, `agy --input-format stream-json`, `codex app-server`, or `gemini --acp`)
after you authenticate with that provider's CLI. It does not implement OAuth flows, read
provider OAuth tokens directly, or call private provider APIs. You are responsible for
choosing an authentication method permitted for your account, plan, and usage pattern.

## Subagent Limits

Agentty requests a limit of two concurrent subagents per Codex or Claude session,
excluding the parent agent. It applies the limit when starting the provider process,
including when resuming a saved session. Restart Agentty after updating to apply the
limit to existing sessions.

Codex limits concurrently open child-agent threads. Claude limits ordinary subagent
spawning, but provider exceptions such as resumed subagents can exceed the limit.
Enforcement depends on the installed CLI supporting its native setting. Gemini and
Antigravity have no verified concurrency setting, so Agentty does not cap their internal
subagents.

These are per-session provider limits, not host-wide CPU or memory limits. Multiple
sessions each have their own allowance, and builds or tests can consume additional
resources.

## Authentication and Usage

### Codex

<a id="backends-codex-authentication"></a> Codex is the recommended backend when you
want subscription-backed usage. The CLI supports signing in with ChatGPT through
`codex login`, and Agentty uses the supported `codex app-server` integration surface.
Usage remains subject to the
[OpenAI Terms of Use](https://openai.com/policies/terms-of-use/).

### Claude

<a id="backends-claude-authentication"></a> For Agentty usage through `claude -p`, use
API-key authentication through Claude Console or a supported cloud provider instead of a
Claude Free, Pro, or Max subscription sign-in. Anthropic's
[Claude Code legal and compliance documentation](https://code.claude.com/docs/en/legal-and-compliance)
describes subscription OAuth as intended for Claude Code and native Anthropic
applications, while developer integrations should use API keys or supported cloud
providers.

If Claude session turns or utility prompts fail with `authentication_error`,
`Failed to authenticate`, or `OAuth token has expired`, refresh the CLI session and
retry:

```bash
claude auth login
claude auth status
```

### Antigravity

<a id="backends-antigravity-authentication"></a> For Agentty usage through `agy`
headless mode, prefer authentication backed by a Google Cloud project or API key rather
than Google Account subscription sign-in. The
[Antigravity terms](https://antigravity.google/terms) do not currently explain how
subscription access applies when third-party tools invoke headless sessions.

Antigravity retains conversation context between turns and resumes it after a restart.

### Gemini

<a id="backends-gemini-authentication"></a> Google Account OAuth no longer works for
Gemini CLI after Google's
[transition from Gemini CLI to Antigravity CLI](https://developers.googleblog.com/an-important-update-transitioning-gemini-cli-to-antigravity-cli/).
Use `GEMINI_API_KEY` or Vertex AI authentication, or choose the Antigravity backend
instead.

## Project Instruction Files

<a id="backends-project-instruction-files"></a> Agentty relies on each backend's native
project-instruction discovery instead of inlining repository guidance into prompts.

- Codex loads `AGENTS.md`.
- Claude Code loads `CLAUDE.md`.
- Gemini CLI loads `GEMINI.md`.
- Antigravity CLI loads `AGENTS.md` and `GEMINI.md` from the active workspace.

Keeping `CLAUDE.md` and `GEMINI.md` as symlinks to a canonical `AGENTS.md` gives all
backends the same repository guidance.

## Selecting a Backend

<a id="backends-selecting-a-backend"></a> Use `/model` to choose a locally available
backend, then one of its models.

The **Projects** tab shows installed CLI versions and `updating...` during startup
refresh. Antigravity, Claude, and Codex use their native updaters; Gemini updates only
when Agentty recognizes an npm-global installation. If an Antigravity executable changes
while Agentty is running, wait for discovery or restart before retrying a turn.

<a id="backends-persistent-defaults"></a> In **Settings**, choose a model and reasoning
level for each project role. Claude and Codex also offer `Normal` or `Fast` response
speed. Unavailable backend defaults fall back to an installed backend.

<a id="backends-reasoning-level"></a>

| Role   | Used for                   |
| ------ | -------------------------- |
| Smart  | New sessions               |
| Fast   | Titles and commit messages |
| Review | Focused reviews            |

Session overrides take precedence. Changing defaults does not alter existing sessions.
`Default Response Style` initializes new sessions as `Concise`, `Balanced`, or
`Detailed`.

Antigravity maps `xhigh` and `max` reasoning to `high`. Claude maps both to `max`, which
is supported by `claude-opus-5-5`. Codex supports a distinct `max` effort.

## Available Models

<a id="backends-available-models"></a> Each backend exposes one or more selectable model
entries with different trade-offs between speed, quality, and cost.

### Antigravity and Gemini Models

Both providers share the same Gemini model ids:

- `gemini-3.1-pro-preview` (default): Higher-quality Gemini model for deeper reasoning.
- `gemini-3.8-flash`: Fast Gemini model for agentic and multimodal tasks.
- `gemini-3.5-flash-lite`: Lightweight Gemini model for fast, cost-conscious workloads.

### Claude Models

- `claude-fable-5` (default): Claude Fable model for creative, narrative-heavy tasks.
- `claude-opus-5-5`: Latest Claude Opus model for complex agentic tasks.
- `claude-sonnet-5`: Balanced Claude model for quality and latency.
- `claude-haiku-4-5-20251001`: Fast Claude model for lighter tasks.

### Codex Models

- `gpt-6-astra`: Most capable Codex model for the hardest end-to-end work.
- `gpt-6-sol` (default): Codex model for complex coding and agentic workflows.
- `gpt-6-luna`: Efficient Codex model for focused, high-volume tasks.
- `gpt-5.6-terra`: Current Codex model for balanced coding performance.
- `gpt-5.3-codex-spark`: Codex spark model for quick coding iterations.

### Stored Model Upgrades

Model pickers show only the current models listed above. When a stored project default
or active session references a superseded model, Agentty upgrades and persists its
replacement automatically. Finished sessions preserve their historical model data.

## Switching Models

<a id="backends-switching-models"></a> Use `/model` to change the current session's
backend and model, `/reasoning` to change reasoning effort, and `/style` to choose
answer length. See [Slash Commands](@/docs/usage/workflow.md#slash-commands) for
response styles and permission modes.

Claude and Codex also support `/speed`. `Fast` reduces latency at higher provider cost
and may switch to a compatible model:

| Selection   | Model used with Fast |
| ----------- | -------------------- |
| Claude      | `claude-opus-5-5`    |
| Codex Spark | `gpt-6-sol`          |

Returning to `Normal` keeps the resulting model. Choosing an incompatible model resets
speed to `Normal`. These changes do not alter project defaults. Gemini and Antigravity
have no speed control. See the provider guides for
[Codex fast mode](https://learn.chatgpt.com/docs/agent-configuration/speed) and
[Claude Code fast mode](https://code.claude.com/docs/en/fast-mode).

<a id="backends-switching-default-model"></a> To change defaults for future sessions,
use **Settings**. See [Settings Scope](@/docs/usage/workflow.md#settings-scope).
