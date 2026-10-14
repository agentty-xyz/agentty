+++
title = "Scheduler"
description = "How Agentty admits session turns."
weight = 2
+++

`ag-scheduler` provides shared admission. A shared admission pool bounds concurrent
session model turns to eight per Agentty process. Regular and stacked sessions use the
same pool. A turn waits for capacity after its session operation is accepted;
cancellation ends that wait. A canceled running turn retains its turn slot through
provider shutdown. Shutdown outside a running turn uses a separate pool of eight cleanup
slots. A canceled request can wait for a cleanup slot, then allows up to five seconds
for provider shutdown before reporting interruption. Each session's worker still orders
its own messages and commands.

Utility runs retain their separate bounded capacity so a session can await a nested
utility without waiting behind itself.

Scheduler permits exist only in the running process. Persisted session operations remain
the recovery authority. On restart, Agentty reconciles unfinished operations before
admitting new work; it does not replay a turn that may have started. See
[Execution](@/docs/core-components/execution.md) for the worker and harness boundary.
