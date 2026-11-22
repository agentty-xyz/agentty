+++
title = "Architecture"
description = "Design and architecture references for Agentty internals."
weight = 5
sort_by = "weight"
+++

<a id="architecture-overview"></a> <a id="architecture-introduction"></a> Design and
architecture references for Agentty runtime flow, module ownership, testability
boundaries, orchestration, and change paths.

## Architecture Topics

- [Runtime Flow](@/docs/architecture/runtime-flow.md) explains the event loop, session
  execution, background work, and recovery.
- [Module Map](@/docs/architecture/module-map.md) maps crates and `agentty` layers to
  their responsibilities.
- [Change Recipes](@/docs/architecture/change-recipes.md) gives architecture-safe change
  paths for common contribution scenarios.
- [Testability Boundaries](@/docs/architecture/testability-boundaries.md) documents
  trait boundaries and deterministic testing guidance for external integrations.
- [`ag-harness` Design](@/docs/architecture/ag-harness-design.md) documents the native
  model loop, durable sessions, tools, and provider transports.
- [Execution Graphs](@/docs/architecture/execution-graphs.md) documents static graph
  snapshots, dispatch, exact result handoffs, and recovery.
- [Harness Graph Integration](@/docs/architecture/harness-graph-integration.md) proposes
  how execution graph nodes run on the `ag-harness` run API.
- [Remote Control Design](@/docs/architecture/remote-control-design.md) documents the
  remote-control transports, keys, relay trust model, and design decisions.
