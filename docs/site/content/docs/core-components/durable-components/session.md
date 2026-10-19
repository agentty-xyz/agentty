+++
title = "Session"
description = "Persistent conversation and workspace context for ongoing work."
weight = 3
+++

A Session holds the conversation and workspace context for work within a
[project](@/docs/core-components/durable-components/project.md). Its saved state
includes execution preferences, lifecycle progress, and its relationship to the working
branch and workspace.

## Execution Environment

`EnvId::SessionWorktree` identifies the local workspace owned by a session using its
full persisted session ID. `Session::env_id()` and `Task::env_id()` expose the same
identity for every task in that session. Existing session ownership supplies this
binding without a separate environment record or migration.

The Env is shared among the session's tasks, while remaining isolated from the main
repository checkout. The host resolves a `ResolvedEnv` with the validated branch,
working directory, and protected main checkout before a turn executes. Bare repositories
have no main checkout to protect. Identity alone does not mean a workspace is ready:
draft setup, missing-directory failures, and isolation checks retain their existing
behavior.

The session owns provisioning, branch operations, and cleanup. Task completion does not
remove the workspace; attempts remain serialized through the session worker. The
session's existing project and base-branch metadata remain authoritative; resolution
does not freeze a new base revision or move a branch. Env does not select an agent,
create a separate conversation, or grant permissions. Per-task worktree provisioning and
remote execution are not implemented.

## Continuity

[Messages](@/docs/core-components/durable-components/message.md) preserve the session's
history, while [operations](@/docs/core-components/durable-components/operation.md)
track workflow execution. A session can span many turns and operations; it is not a
single execution attempt.

Saved context remains available after restart. Interrupted execution is reconciled by
the [worker](@/docs/core-components/execution.md), rather than automatically repeated.
