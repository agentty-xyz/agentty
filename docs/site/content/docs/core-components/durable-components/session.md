+++
title = "Session"
description = "Persistent conversation and workspace context for ongoing work."
weight = 3
+++

A Session holds the conversation and workspace context for work within a
[project](@/docs/core-components/durable-components/project.md). Its saved state
includes execution preferences, lifecycle progress, and its relationship to the working
branch and workspace.

## Continuity

[Messages](@/docs/core-components/durable-components/message.md) preserve the session's
history, while [operations](@/docs/core-components/durable-components/operation.md)
track workflow execution. A session can span many turns and operations; it is not a
single execution attempt.

Saved context remains available after restart. Interrupted execution is reconciled by
the [worker](@/docs/core-components/execution.md), rather than automatically repeated.
