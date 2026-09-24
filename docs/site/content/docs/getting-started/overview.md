+++
title = "Overview"
description = "Understand how Agentty organizes AI agent sessions and workflows."
weight = 0
+++

<a id="overview-introduction"></a> `agentty` is an ADE (Agentic Development Environment)
for AI-assisted software development in your terminal.

<a id="overview-ai-sessions"></a> It runs AI coding agents in dedicated AI sessions.

<!-- more -->

## What Agentty Provides

<a id="overview-operational-lift"></a> When you start a session, Agentty:

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

The session starts from your local active branch, even if it is behind the remote. Sync
the project first to include remote changes. Agentty checks the worktree before each
turn and keeps session edits separate from your base branch until you merge.
Bare-repository projects are also supported.

<a id="overview-worktree-cleanup"></a> Worktrees are stored under `~/.agentty/wt/` and
are cleaned up automatically when a session reaches `Done` or `Canceled`, or when you
delete a session.

## Auto-Update

<a id="overview-auto-update"></a> Agentty checks for updates at startup and hourly.
Installed agent CLIs are also refreshed at startup. See
[Auto-Update](@/docs/usage/workflow.md#auto-update) for status messages and controls.

## Key Concepts

- **Agent**: An external AI CLI backend (Antigravity, Claude, Codex, or Gemini) that
  performs coding work. See [Agents & Models](@/docs/agents/backends.md).
- **Session**: An isolated unit of work: a conversation, a worktree branch, and a
  reviewable diff. See [Workflow](@/docs/usage/workflow.md) and
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
