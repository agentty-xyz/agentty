+++
title = "Agents & Models"
description = "Supported agent backends, available models, and how to configure them."
weight = 1
+++

<a id="backends-introduction"></a> Agentty delegates coding work to external AI agent
CLIs. Each backend is a standalone CLI tool that Agentty launches in an isolated
worktree. This page covers the supported backends, available models, and configuration
options.

<!-- more -->

## Supported Backends

<a id="backends-supported-backends"></a> Agentty supports four agent backends. Each
requires its respective CLI to be installed and available on your `PATH`.

- Codex (`codex`, recommended; supports subscription usage): install the
  [Codex CLI](https://github.com/openai/codex), then run `codex login`.
- Claude (`claude`): install [Claude Code](https://github.com/anthropics/claude-code),
  then run `claude auth login`.
- Antigravity (`agy` 1.1.0 or newer): install the
  [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli), then run
  `agy` and follow its sign-in flow.
- Gemini (`gemini`): install the
  [Gemini CLI](https://github.com/google-gemini/gemini-cli) and authenticate with an API
  key or Vertex AI.

All backends accept pasted local prompt images from the Agentty composer (`Ctrl+V`,
`Ctrl+Shift+V`, or `Alt+V` in prompt mode) and run their turns non-interactively inside
the session worktree.

Codex can edit repository-local `.agents/` content inside that worktree, including
project skills and instructions. Its `.git` and `.codex/` paths remain read-only during
normal turns.

Claude turns also allow Claude Code's `WebSearch` and `WebFetch` tools, so prompts that
need current external information can use the web without an interactive permission
grant. File edits remain scoped to the session worktree.

Treat fetched web content as untrusted context. Claude still has edit-capable tools
during the turn, so keep web-backed prompts specific and review the session diff before
merging.

Agentty requires at least one supported backend CLI on `PATH` at startup and fails with
an install hint when none is found.

Agentty uses each provider's official non-interactive CLI or app-server surface
(`claude -p`, `agy --print`, `codex app-server`, or `gemini --acp`) after you
authenticate with that provider's CLI. It does not implement OAuth flows, read provider
OAuth tokens directly, or call private provider APIs. You are responsible for choosing
an authentication method permitted for your account, plan, and usage pattern.

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

<a id="backends-antigravity-authentication"></a> For Agentty usage through
`agy --print`, prefer authentication backed by a Google Cloud project or API key rather
than Google Account subscription sign-in. The
[Antigravity terms](https://antigravity.google/terms) do not currently explain how
subscription access applies when third-party tools invoke `agy --print`.

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

<a id="backends-selecting-a-backend"></a> Choose the backend from the `/model` picker:

```bash
# Open model selection (backend first, then model)
/model
```

The picker is filtered to the backend CLIs currently available on the machine. If only
`agy` is installed, `/model` shows only Antigravity and its selectable Gemini model
choices.

At startup, Agentty refreshes each available agent CLI in the background, then probes
`--version` and updates the Projects tab's **Agent CLIs** rows with the current version.
Antigravity, Claude, and Codex use their native `update` commands. Because current
Gemini CLI releases do not expose that command, npm-global Gemini installations are
refreshed with `npm install -g @google/gemini-cli@latest`. Rows show `updating...` until
the refresh completes. Gemini installations that Agentty cannot identify as npm-global
are version-probed without an automatic update.

<a id="backends-persistent-defaults"></a> For persistent defaults, choose a default
model in the **Settings** tab (`Tab` to navigate, `Enter` to open the selector). The
selected model is stored with its backend as `agent/model`, which keeps shared Gemini
model ids tied to the selected Gemini or Antigravity provider. Stored defaults that
point at an unavailable backend fall back to the first available backend default.

<a id="backends-reasoning-level"></a> For Antigravity, Codex, and Claude sessions, the
**Settings** tab also exposes `Default Reasoning Level` (`low`, `medium`, `high`,
`xhigh`, `max`). The selected level is persisted per project and is sent with turns
unless a session-specific override is active. Antigravity receives `--effort low`,
`--effort medium`, or `--effort high`; `xhigh` and `max` map to its highest supported
value, `--effort high`. Codex receives `max` as a distinct reasoning effort. For Claude,
both `xhigh` and `max` map to `--effort max`, which is currently only supported by
`claude-opus-4-8`.

## Available Models

<a id="backends-available-models"></a> Each backend exposes one or more selectable model
entries with different trade-offs between speed, quality, and cost.

### Antigravity and Gemini Models

Both providers share the same Gemini model ids:

- `gemini-3.1-pro-preview` (default): Higher-quality Gemini model for deeper reasoning.
- `gemini-3.6-flash`: Fast Gemini model for agentic and multimodal tasks.
- `gemini-3.5-flash-lite`: Lightweight Gemini model for fast, cost-conscious workloads.
- `gemini-3-flash-preview`: Fast Gemini model for quick iterations.

### Claude Models

- `claude-fable-5` (default): Claude Fable model for creative, narrative-heavy tasks.
- `claude-opus-4-8`: Latest Claude Opus model for complex tasks.
- `claude-sonnet-5`: Balanced Claude model for quality and latency.
- `claude-haiku-4-5-20251001`: Fast Claude model for lighter tasks.

### Codex Models

- `gpt-5.6-sol` (default): Newest Codex model for the strongest coding performance.
- `gpt-5.6-terra`: Current Codex model for balanced coding performance.
- `gpt-5.6-luna`: Current Codex model for lighter coding iterations.
- `gpt-5.5`: Newer Codex model with stronger coding performance when available.
- `gpt-5.3-codex-spark`: Codex spark model for quick coding iterations.

Stored project defaults or session rows that reference a retired model id (such as
`gemini-3.5-flash`, `gemini-3.1-flash-lite-preview`, `claude-opus-4-6`,
`claude-opus-4-7`, `claude-sonnet-4-6`, `gpt-5.4`, or `gpt-5.4-mini`) are upgraded to
the current supported replacement when Agentty loads them.

## Switching Models

<a id="backends-switching-models"></a> You can switch the model for the current session
using the `/model` slash command in the prompt input. This opens a two-step picker:
first choose the backend, then choose one of its models. Both steps are filtered to
locally available backends.

You can also switch the reasoning level for the current session with the `/reasoning`
slash command. The picker preselects the current effective reasoning level.

<a id="backends-switching-default-model"></a> To change the **default model**
persistently, use the **Settings** tab (`Tab` to navigate to it, `Enter` to open the
selector).
