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

<a id="overview-operational-lift"></a> For every session, Agentty:

- Creates an isolated worktree branch for agent edits.
- Keeps agent output, diffs, and generated changes in one reviewable stream.
- Keeps published session branches updated after later completed turns.

## Typical Flow

A session takes your prompt through agent edits to a reviewable diff; you decide whether
to merge or discard it. Follow
[Installation](@/docs/getting-started/installation.md#start-a-session) for your first
session. Sessions can also be staged as drafts; see
[Workflow](@/docs/usage/workflow.md).

## Worktree Isolation

<a id="overview-worktree-isolation"></a> Every session runs in its own
[git worktree](https://git-scm.com/docs/git-worktree), created from your local base
branch even if it is behind the remote; sync the project first to include remote
changes. Session edits stay separate from your base branch until you merge.
Bare-repository projects are also supported.

<a id="overview-worktree-cleanup"></a> Worktrees are stored under `~/.agentty/wt/` and
removed when a session reaches `Done` or `Canceled`, or when you delete it.

## Auto-Update

<a id="overview-auto-update"></a> Agentty checks for its own updates at startup and
hourly, and refreshes installed agent CLIs at startup. See
[Auto-Update](@/docs/usage/workflow.md#auto-update).

## Key Concepts

- **Agent**: An external AI CLI backend (Antigravity, Claude, or Codex) that performs
  coding work. See [Agents & Models](@/docs/agents/backends.md).
- **Session**: An isolated unit of work: a conversation, a worktree branch, and a
  reviewable diff. See [Workflow](@/docs/usage/workflow.md) and
  [Keybindings](@/docs/usage/keybindings.md).
- **Project**: A Git repository registered in Agentty, selected in the **Projects** tab.
- **Diff view**: The changes an agent made in a session, ready for your review.

## Development Status

Agentty is in active development. Releases follow Semantic Versioning, but the current
`0.y.z` series may still introduce breaking changes. Always review and verify changes
before relying on them in your repositories.

## Next Steps

- [Installation](@/docs/getting-started/installation.md) — install Agentty and run it
  for the first time.
- [Agents & Models](@/docs/agents/backends.md) — configure backends and choose models.
- [Workflow](@/docs/usage/workflow.md) — learn the interface layout and session
  lifecycle.
- [Keybindings](@/docs/usage/keybindings.md) — learn the keyboard shortcuts for each
  view.
