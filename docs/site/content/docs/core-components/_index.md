+++
title = "Core Components"
description = "The components that power Agentty and how they connect."
weight = 4
sort_by = "weight"
+++

Agentty's core has three groups: a control plane that admits work, execution components
that run model work, and durable components that survive restarts. Application workflows
and the TUI sit on top of them.

- [Component Map](@/docs/core-components/component-map.md): how the groups and their
  components connect.
- [Control Plane](@/docs/core-components/control-plane/_index.md): command admission and
  shared capacity.
- [Execution Components](@/docs/core-components/execution-components/_index.md): run
  workers, the agent runtime, harnesses, and LLMs.
- [Durable Components](@/docs/core-components/durable-components/_index.md): persisted
  project, conversation, and execution records.
