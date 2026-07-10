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

- Gemini (`gemini`): Google Gemini CLI agent.
- Antigravity (`agy` 1.1.0 or newer): Google Antigravity CLI agent.
- Claude (`claude`): Anthropic Claude Code agent.
- Codex (`codex`): OpenAI Codex CLI agent.

All backends accept pasted local prompt images from the Agentty composer (`Ctrl+V`,
`Ctrl+Shift+V`, or `Alt+V` in prompt mode) and run their turns non-interactively inside
the session worktree.

Claude turns also allow Claude Code's `WebSearch` and `WebFetch` tools, so prompts that
need current external information can use the web without an interactive permission
grant. File edits remain scoped to the session worktree.

Treat fetched web content as untrusted context. Claude still has edit-capable tools
during the turn, so keep web-backed prompts specific and review the session diff before
merging.

Agentty requires at least one supported backend CLI on `PATH` at startup and fails with
an install hint when none is found.

## Project Instruction Files

<a id="backends-project-instruction-files"></a> Agentty relies on each backend's native
project-instruction discovery instead of inlining repository guidance into prompts.

- Codex loads `AGENTS.md`.
- Claude Code loads `CLAUDE.md`.
- Gemini CLI loads `GEMINI.md`.
- Antigravity CLI loads `AGENTS.md` and `GEMINI.md` from the active workspace.

Keeping `CLAUDE.md` and `GEMINI.md` as symlinks to a canonical `AGENTS.md` gives all
backends the same repository guidance.

## Claude Authentication

<a id="backends-claude-authentication"></a> If Claude session turns or utility prompts
fail with `authentication_error`, `Failed to authenticate`, or
`OAuth token has expired`, refresh the Claude CLI session and retry:

```bash
claude auth login
claude auth status
```

For SSO-backed accounts, use `claude auth login --sso`.

## Selecting a Backend

<a id="backends-selecting-a-backend"></a> Choose the backend from the `/model` picker:

```bash
# Open model selection (backend first, then model)
/model
```

The picker is filtered to the backend CLIs currently available on the machine. If only
`agy` is installed, `/model` shows only Antigravity and its selectable Gemini model
choices.

At startup, Agentty runs each available agent CLI's `update` command in the background,
then probes `--version` and refreshes the Projects tab's **Agent CLIs** rows with the
current version. Rows show `updating...` until that refresh completes.

<a id="backends-persistent-defaults"></a> For persistent defaults, choose a default
model in the **Settings** tab (`Tab` to navigate, `Enter` to open the selector). The
selected model is stored with its backend as `agent/model`, which keeps shared Gemini
model ids tied to the selected Gemini or Antigravity provider. Stored defaults that
point at an unavailable backend fall back to the first available backend default.

<a id="backends-reasoning-level"></a> For Codex and Claude sessions, the **Settings**
tab also exposes `Default Reasoning Level` (`low`, `medium`, `high`, `xhigh`). The
selected level is persisted per project and is sent with turns unless a session-specific
override is active. For Claude, `xhigh` maps to `--effort max`, which is currently only
supported by `claude-opus-4-8`.

## Available Models

<a id="backends-available-models"></a> Each backend exposes one or more selectable model
entries with different trade-offs between speed, quality, and cost.

### Antigravity and Gemini Models

Both providers share the same Gemini model ids:

- `gemini-3.1-pro-preview` (default): Higher-quality Gemini model for deeper reasoning.
- `gemini-3.5-flash`: Fast Gemini model for current Flash workloads.
- `gemini-3.1-flash-lite-preview`: Lightweight Gemini model for fast, cost-conscious
  iterations.
- `gemini-3-flash-preview`: Fast Gemini model for quick iterations.

### Claude Models

- `claude-fable-5` (default): Claude Fable model for creative, narrative-heavy tasks.
- `claude-opus-4-8`: Latest Claude Opus model for complex tasks.
- `claude-sonnet-5`: Balanced Claude model for quality and latency.
- `claude-haiku-4-5-20251001`: Fast Claude model for lighter tasks.

### Codex Models

- `gpt-5.5` (default): Newer Codex model with stronger coding performance when
  available.
- `gpt-5.4-mini`: Small, fast Codex model for simpler coding tasks.
- `gpt-5.3-codex-spark`: Codex spark model for quick coding iterations.

Stored project defaults or session rows that reference a retired model id (such as
`claude-opus-4-6`, `claude-opus-4-7`, `claude-sonnet-4-6`, or `gpt-5.4`) are upgraded to
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
