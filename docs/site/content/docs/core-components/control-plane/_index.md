+++
title = "Control Plane"
description = "How Agentty admits commands and bounds concurrent model work."
weight = 1
sort_by = "weight"
+++

The control plane decides which work is accepted and when it may run. It never runs
model work itself; that belongs to the
[execution components](@/docs/core-components/execution-components/execution-boundary.md).

- [Command Gateway](@/docs/core-components/control-plane/command-gateway.md): shared
  admission for terminal and programmatic session commands.
- [Scheduler](@/docs/core-components/control-plane/scheduler.md): shared capacity for
  session turns.
