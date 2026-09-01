+++
title = "Overview"
description = "Understand how Agentty organizes AI agent sessions and workflows."
weight = 0
+++

<a id="overview-introduction"></a> `agentty` is an ADE (Agentic Development Environment)
for structured, controllable AI-assisted software development. This project is built
using `agentty` itself, including the docs and docs-site you are reading.

<a id="overview-ai-sessions"></a> It runs AI coding agents in dedicated AI sessions.

<!-- more -->

## What Agentty Provides

<a id="overview-operational-lift"></a> When you start a session, Agentty does the
operational heavy lifting for workflow safety:

- Spawns a clean worktree branch for every live session.
- Runs agent-driven edits in isolation from your base branch.
- Keeps terminal output, diffs, and generated changes in one reviewable stream.
- Keeps already-published session branches synced after later completed turns.

## Typical Flow

1. Open a repository and start `agentty`.
1. Press `a` and choose `Regular`.
1. Type the first prompt and press `Enter` to start the agent immediately.
1. Let the agent modify files in its worktree.
1. Review the diff (`d`) and decide to merge or discard.

Sessions can also be staged as drafts or stacked on top of another session. See
[Workflow](@/docs/usage/workflow.md) for draft and stacked session details.

## Worktree Isolation

<a id="overview-worktree-isolation"></a> Every session runs in its own
[git worktree](https://git-scm.com/docs/git-worktree), created automatically when the
live session starts:

- The worktree branch is named `wt/<hash>`, where `<hash>` is derived from the session
  ID.
- The branch starts from whichever local branch was active when you launched `agentty`.
  If local `main` is behind `origin/main`, the session still starts from local `main`.
- All agent edits happen inside the worktree, keeping your base branch untouched until
  you explicitly merge.
- Before each turn, Agentty verifies that the session folder still exists, is on its
  expected `wt/<hash>` branch, and resolves to a linked worktree rather than the main
  checkout.
- Projects backed by a bare repository (a container folder holding per-branch worktrees,
  with no main working checkout) are supported. When there is no main checkout, the
  per-turn dirty-state guard that inspects the main checkout is skipped.
- Agent prompts repeat that the session worktree is the only writable root for normal
  turns.
- If worktree creation fails (e.g., git is not installed or permissions are
  insufficient), session creation fails atomically and displays an error.

<a id="overview-worktree-cleanup"></a> Worktrees are stored under `~/.agentty/wt/` and
are cleaned up automatically when a session reaches `Done` or `Canceled`, or when you
delete a session.

## Auto-Update

<a id="overview-auto-update"></a> Agentty checks npmjs for newer versions at startup and
once every hour while running, then automatically installs updates in the background.
Agent backend CLIs (`agy`, `claude`, `codex`, and `gemini`) are also refreshed on
startup when installed. See [Workflow](@/docs/usage/workflow.md) for status-bar details
and how to disable automatic updates with `--no-update`.

## Key Concepts

- **Agent**: An external AI CLI backend (Antigravity, Claude, Codex, or Gemini) that
  performs coding work. See [Agents & Models](@/docs/agents/backends.md).
- **Session**: An isolated unit of work: one prompt, one worktree branch, one reviewable
  diff. See [Workflow](@/docs/usage/workflow.md) and
  [Keybindings](@/docs/usage/keybindings.md).
- **Project**: A git repository registered in Agentty. Select between projects with the
  Projects tab.
- **Diff view**: Press `d` in a review-state session to see exactly what the agent
  changed.

## Development Status

Agentty is in active development. Releases follow Semantic Versioning, but the current
`0.y.z` series may still introduce breaking changes as workflows, integrations, and
safeguards evolve. Always review and verify changes before relying on them in your
repositories.

## Next Steps

- [Installation](@/docs/getting-started/installation.md) — install Agentty and run it
  for the first time.
- [Agents & Models](@/docs/agents/backends.md) — configure backends and choose models.
- [Workflow](@/docs/usage/workflow.md) — learn the interface layout and session
  lifecycle.
- [Keybindings](@/docs/usage/keybindings.md) — learn the keyboard shortcuts for each
  view.
