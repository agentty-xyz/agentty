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

<a id="backends-supported-backends"></a> Agentty supports three agent backends. Each
requires its respective CLI to be installed and available on your `PATH`.

| Backend | CLI command | Description | |---------|-------------|-------------| |
Antigravity | `agy` | Google Antigravity CLI agent. | | Claude | `claude` | Anthropic
Claude Code agent. | | Codex | `codex` | OpenAI Codex CLI agent. |

All supported session backends accept pasted local prompt images from the Agentty
composer (`Ctrl+V`, `Ctrl+Shift+V`, or `Alt+V` in prompt mode). Transport details differ
by backend:

- Codex app-server turns send `localImage` input items in placeholder order.
- Claude Code turns receive the prompt over stdin with `[Image #n]` placeholders
  rewritten to local image paths that Claude can inspect.
- Antigravity CLI turns receive the prompt over stdin with `[Image #n]` placeholders
  rewritten to local image paths. Agentty passes the session worktree first through
  `agy --add-dir`, followed by any local image parent directories, so Antigravity tools
  keep the session worktree as the editable workspace root. When the real worktree path
  contains a hidden directory such as `.agentty`, Agentty gives `agy` a non-hidden temp
  symlink alias for that worktree because Antigravity refuses hidden workspace roots.
  Session teardown removes that alias so the system temp directory does not accumulate
  stale Antigravity workspace links.

Codex now always runs through `codex app-server`, including isolated utility prompts
such as title generation, review assist, commit-message generation, and auto-commit
recovery. Session rebase-conflict assistance runs through the existing session channel
so the provider keeps conversation context. Agentty no longer uses a direct `codex exec`
path.

## Project Instruction Files

<a id="backends-project-instruction-files"></a> Agentty relies on each backend's native
project-instruction discovery instead of inlining repository guidance into prompts.

- Codex loads `AGENTS.md`.
- Claude Code loads `CLAUDE.md`.
- Antigravity CLI loads `AGENTS.md` and `GEMINI.md` from the active workspace.

This repository keeps `CLAUDE.md` and `GEMINI.md` as symlinks to the canonical root
`AGENTS.md`, and keeps additional `AGENTS.md` files only at major module boundaries.
This gives all backends shared repo-wide instructions plus a small amount of
higher-signal local guidance without maintaining per-directory file inventories.

Shared reusable skills live under `skills/`. For backends that support workspace skills,
the preferred project-local alias is `.agents/skills`; Gemini CLI documents this as an
alias for `.gemini/skills`. Claude Code still documents project-specific commands,
agents, settings, and skills under `.claude`, while Codex still documents project
configuration under `.codex`, so Agentty keeps those backend-specific directories
available when compatibility requires them instead of renaming them wholesale.

## Claude Authentication

<a id="backends-claude-authentication"></a> If Claude session turns or utility prompts
fail with `authentication_error`, `Failed to authenticate`, or
`OAuth token has expired`, refresh the Claude CLI session and retry:

```bash
claude auth login
claude auth status
```

For SSO-backed accounts, use `claude auth login --sso`.

## File Path Output Format

<a id="backends-path-output-format"></a> Agentty prompts all backends to reference files
using repository-root-relative POSIX paths. This keeps file references consistent in
session output and reviews. The rule is carried by the shared Askama markdown prompt
templates under `crates/agentty/src/infra/agent/template/`, with
`protocol_instruction_prompt.md` owning the full bootstrap wrapper and
`protocol_refresh_prompt.md` owning the compact app-server reminder.

- Allowed forms: `path`, `path:line`, `path:line:column`
- Example: `crates/agentty/src/infra/agent/prompt.rs:48`
- Not allowed: absolute paths, `file://` URIs, or `../`-prefixed paths

## Structured Response Protocol

<a id="backends-structured-response-protocol"></a> Agentty prepends one shared protocol
preamble from `crates/agentty/src/infra/agent/template/protocol_instruction_prompt.md`.
That template contains the repository-root-relative file path rules, the structured
response instructions, the explicit `---` separator that separates the task body, and
the full self-descriptive JSON Schema generated from the protocol subsystem in
`crates/agentty/src/infra/agent/protocol.rs`. Profile-specific usage guidance now lives
in sibling markdown templates: `protocol_instruction_session_turn_usage.md` and
`protocol_instruction_utility_prompt_usage.md`. Compact app-server refresh prompts are
rendered from `protocol_refresh_prompt.md` plus the matching profile-specific reminder
template. The router delegates to `protocol/model.rs`, `protocol/schema.rs`, and
`protocol/parse.rs`, while `crates/agentty/src/infra/agent/prompt.rs` owns the shared
prompt-preparation path used by CLI and app-server turns.

Each request path now selects one canonical `AgentRequestKind` before the backend sees
the prompt, and the backend derives the protocol-owned `ProtocolRequestProfile` from
that request kind:

- Session turns use `AgentRequestKind::SessionStart` or
  `AgentRequestKind::SessionResume`, which both derive the `SessionTurn` profile.
- One-shot utility prompts use `AgentRequestKind::UtilityPrompt`, which derives the
  `UtilityPrompt` profile.
- Strict and permissive request paths still share the same protocol contract after that
  derivation step.

Persistent app-server session turns no longer resend that full prompt wrapper on every
follow-up. Agentty now tracks an instruction-profile bootstrap marker per stored
`provider_conversation_id` and switches among three delivery modes:

- `BootstrapFull`: first turn in a provider context sends the full preamble plus schema.
- `DeltaOnly`: later Codex follow-up turns in the same restored provider context send
  only a compact reminder of the existing file-path and JSON contract.
- `BootstrapWithReplay`: runtime restarts or context resets resend the full contract and
  pair it with transcript replay when provider context was not restored.

The shared schema defines a top-level `answer` markdown string, a `questions` array, and
the optional top-level `summary` object. Session turns typically populate:

- `summary.turn` describes only the work completed in the current turn
- `summary.session` describes the cumulative session-branch diff that still applies

Utility prompts, such as title generation, session commit-message generation, focused
review preparation, auto-commit assistance, and conflict assistance, still return the
same protocol JSON shape. They may leave `summary` unused, while session discussion
turns typically populate it. Final parsing accepts any payload that deserializes to the
shared protocol wire type, so session-turn responses can carry meaning in `summary` even
when `answer` is blank and `questions` is empty.

Example payload:

```json
{
  "answer": "Implemented the change.",
  "questions": [
    {
      "text": "Should I run the full test suite?",
      "options": ["Yes", "No", "Only changed files"]
    }
  ],
  "summary": {
    "turn": "- Updated the protocol prompt templates.",
    "session": "- Added mandatory structured summaries to the response contract."
  }
}
```

<a id="backends-structured-response-routing"></a> Top-level `answer` text is appended to
the normal session transcript. Structured `questions` are persisted separately and move
the session to **Question** status so Agentty can collect clarifications in question
input mode. The top-level `summary` object is persisted separately and rendered in the
session summary panel instead of being parsed back out of answer markdown.

## Protocol Validation

<a id="backends-protocol-validation-repair"></a> Agentty validates final agent output
against the structured response protocol.

- Claude and Codex session turns use strict parsing and fail closed when output does not
  match the protocol schema. Antigravity session turns use the same strict parse and one
  protocol-repair retry first, then preserve non-empty plain text as `answer` when
  `agy --print` ignores both schema prompts.
- Strict parsing accepts summary-only protocol payloads because the parser now relies on
  the shared protocol wire type instead of extra top-level field checks.
- One-shot utility prompts use the same strict final validation across both CLI and
  app-server transports. Plain text, blank responses, trailing junk after a schema
  object, and other non-schema output are rejected instead of being coerced into
  `answer`. Provider prose that appears before one final protocol JSON object is now
  tolerated so Claude-style wrapped completions still recover the authoritative payload.
- When strict validation fails, the surfaced error now includes parse-oriented debug
  details such as response sizing, JSON parser location/category, and visible top-level
  keys from any parsed JSON object so malformed provider output is easier to diagnose.
- Provider-specific transport, stdin-vs-argv prompt delivery, strict final parsing, and
  app-server thought-phase handling are centralized in the shared provider registry in
  `crates/agentty/src/infra/agent/provider.rs`.
- Concrete backends in `crates/agentty/src/infra/agent/` now also own app-server client
  selection and runtime command construction, so Codex transport wiring stays with its
  provider-specific implementation instead of top-level `infra/` modules.
- Claude turns use native schema validation via `claude --json-schema` and
  `--output-format stream-json`, so tool/progress events can stream live while the final
  response remains schema-validated.
- Antigravity turns use `agy --print` because the CLI does not currently expose an
  ACP/app-server flag. Agentty streams the full prompt through stdin, runs with
  `--sandbox`, uses `--dangerously-skip-permissions` so non-interactive worktree edits
  can proceed, passes the session worktree or its non-hidden temp symlink alias as the
  first `--add-dir` root, and tries the shared strict protocol parser plus one repair
  retry for final validation. If Antigravity still returns non-empty plain text, Agentty
  keeps that text as `answer` instead of failing the session with an internal schema
  error. Before each Antigravity launch, Agentty adds `.antigravitycli/` and
  `cache/projects.json` to the repository-local git exclude file so Antigravity's
  project configuration state does not appear in session diffs. When a session is
  deleted, canceled, merged, or rolled back after setup failure, Agentty removes the
  matching Antigravity temp symlink alias before deleting the real worktree directory.
- Prompt-side protocol instructions rely on the raw self-descriptive `schemars` metadata
  (`title`, `description`, and related annotations), while transport `outputSchema`
  payloads are normalized separately for provider compatibility. The same prompt
  instructions also restrict any git usage during session turns to read-only commands
  such as `git diff` and `git show`, and explicitly forbid mutating operations such as
  `git commit` or `git push`.
- Antigravity and Claude stream the rendered prompt body through stdin for CLI one-shot
  flows so large diffs and review prompts do not hit OS argv length limits.
- Claude turns pass `--strict-mcp-config`, so only MCP servers explicitly provided by
  Agentty are allowed (none by default).
- Claude turns allow shell execution (`Bash`), file-modifying tools (`Edit`,
  `MultiEdit`, `Write`), plus `EnterPlanMode` and `ExitPlanMode` for unattended worktree
  edits. Agentty runs Claude with the session worktree as the process working directory.
- Codex app-server turns enforce structured output through transport `outputSchema`; the
  same transport is also used for one-shot Codex utility prompts, and prompt
  instructions embed the same full self-descriptive schema for consistency across
  providers.
- Codex app-server turns use the non-interactive `never` approval policy with a
  workspace-write sandbox. If Codex still emits pre-action approval requests, command
  approvals are accepted under that sandbox and file-change approvals are accepted only
  when every declared path stays inside the session worktree.
- Claude always uses structured protocol output, including isolated one-shot utility
  prompts, through native schema enforcement plus prompt instructions.
- Codex app-server turns include `outputSchema` at transport level and still require the
  final assistant payload itself to parse as the shared protocol JSON object.
- Codex keeps transport-level `outputSchema` enforcement even when a follow-up turn uses
  the compact `DeltaOnly` reminder instead of the full prompt-side schema block.
- Partial protocol JSON fragments are suppressed during streaming so raw JSON wrappers
  do not leak into live transcript output.
- Wrapped stream chunks that end in one valid protocol JSON object are reduced to that
  payload's `answer`, so prefatory provider prose is not persisted when recovery
  succeeds.

## Session Resume Behavior

<a id="backends-session-resume"></a> Agentty persists provider-native conversation
identifiers for app-server backends and uses them to restore context after runtime
restarts. It also persists which provider conversation already received the full
bootstrap so restored contexts can keep using the compact reminder path.

- Codex app-server: resumes by stored `threadId` via `thread/resume`, so restored
  threads can keep the existing bootstrap and use the compact reminder on later turns.
- Antigravity CLI: runs each turn through stateless `agy --print`, so Agentty replays
  prior session output in the prompt for follow-up turns instead of using
  `agy --continue`, which resumes the most recent Antigravity conversation globally.

## App-Server Turn Timeout

<a id="backends-app-server-turn-timeout"></a> App-server-backed turns can run for a long
time. Agentty waits up to 4 hours for Codex app-server turn completion by default.
Antigravity is CLI-backed and passes `--print` before `--print-timeout 1h` while the
prompt body streams through stdin.

## Selecting a Backend

<a id="backends-selecting-a-backend"></a> Choose the backend from the `/model` picker:

```bash
# Open model selection (backend first, then model)
/model
```

Agentty now filters that picker to the backend CLIs currently available on the machine.
If only `agy` is installed, `/model` shows only Antigravity and its selectable Gemini
model choices. If none of `agy`, `codex`, or `claude` are installed, Agentty now fails
at startup with an error telling you to install a supported CLI on `PATH`.

At startup, Agentty runs each available agent CLI's `update` command in the background,
then probes `--version` and refreshes the Projects tab's **Agent CLIs** rows with the
current version. Rows show `updating...` until that refresh completes.

<a id="backends-persistent-defaults"></a> For persistent defaults, choose a default
model in the **Settings** tab (`Tab` to navigate, `Enter` to edit). The selected model
determines which backend is used for new sessions. Stored defaults that point at an
unavailable backend automatically fall back to the first available backend default
instead of leaving the selector on a hidden choice.

<a id="backends-reasoning-level"></a> For Codex and Claude sessions, the **Settings**
tab also exposes `Default Reasoning Level` (`low`, `medium`, `high`, `xhigh`). The
selected level is persisted per project and is sent with turns unless a session-specific
override is active. For Claude, `xhigh` maps to `--effort max`, which is currently only
supported by `claude-opus-4-8`.

## Available Models

<a id="backends-available-models"></a> Each backend exposes one or more selectable model
entries with different trade-offs between speed, quality, and cost.

### Antigravity Models

- `gemini-3.1-pro-preview` (default): Higher-quality Gemini model for deeper reasoning.
- `gemini-3.5-flash`: Fast Gemini model for current Flash workloads.
- `gemini-3.1-flash-lite-preview`: Lightweight Gemini model for fast, cost-conscious
  iterations.
- `gemini-3-flash-preview`: Fast Gemini model for quick iterations.

### Claude Models

- `claude-opus-4-8` (default): Latest Claude Opus model for complex tasks.
- `claude-sonnet-4-6`: Balanced Claude model for quality and latency.
- `claude-haiku-4-5-20251001`: Fast Claude model for lighter tasks.

Stored project defaults or session rows that still reference `claude-opus-4-6` or
`claude-opus-4-7` are upgraded to `claude-opus-4-8` when Agentty loads them.

### Codex Models

- `gpt-5.5` (default): Newer Codex model with stronger coding performance when
  available.
- `gpt-5.4-mini`: Small, fast Codex model for simpler coding tasks.
- `gpt-5.3-codex-spark`: Codex spark model for quick coding iterations.

Stored project defaults or session rows that still reference retired `gpt-5.4` are
upgraded to `gpt-5.5` when Agentty loads them.

## Switching Models

<a id="backends-switching-models"></a> You can switch the model for the current session
using the `/model` slash command in the prompt input. This opens a two-step picker:
first choose the backend, then choose one of its models. Both steps are filtered to
locally available backends, and the current session backend remains preselected when it
is still runnable on the current machine.

You can also switch the reasoning level for the current session with the `/reasoning`
slash command. The picker preselects the current effective reasoning level, using the
active project's `Default Reasoning Level` whenever the session does not already have
its own override.

<a id="backends-switching-default-model"></a> To change the **default model**
persistently, use the **Settings** tab (`Tab` to navigate to it, `Enter` to edit).
