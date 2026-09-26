+++
title = "Scheduler"
description = "How Agentty admits session turns and selects campaign tasks."
weight = 2
+++

`ag-scheduler` applies independent limits. A shared admission pool bounds concurrent
session model turns to eight per Agentty process. Regular, stacked, controller, and
managed child sessions use the same pool. A turn waits for capacity after its session
operation is accepted; cancellation ends that wait. A canceled running turn retains its
turn slot through provider shutdown. Shutdown outside a running turn uses a separate
pool of eight cleanup slots. A canceled request can wait for a cleanup slot, then allows
up to five seconds for provider shutdown before reporting interruption. Each session's
worker still orders its own messages and commands.

Campaign task selection uses the campaign's configured parallelism separately. It counts
active children, including those waiting for clarification, and selects planned tasks in
plan order. `ag-orchestration` owns approval, durable task claims, child creation,
verification, and integration. Utility runs retain their separate bounded capacity so a
session can await a nested utility without waiting behind itself.

Scheduler permits exist only in the running process. Persisted session operations and
campaign task claims remain the recovery authority. On restart, Agentty reconciles
unfinished operations before admitting new work; it does not replay a turn that may have
started. See [Execution](@/docs/core-components/execution.md) for the worker and harness
boundary and [Orchestrator Design](@/docs/architecture/orchestrator.md) for campaign
lifecycle rules.
