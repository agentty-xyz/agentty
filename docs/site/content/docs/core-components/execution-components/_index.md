+++
title = "Execution Components"
description = "The worker-owned path every model request follows."
weight = 2
sort_by = "weight"
+++

Execution components run model work on behalf of application workflows.

- [Execution Boundary](@/docs/core-components/execution-components/execution-boundary.md):
  the mandatory path, its crates, and the dependency rule.
- [Run Worker](@/docs/core-components/execution-components/run-worker.md): admission,
  ordering, cancellation, recovery, and execution policy.
- [Agent Runtime](@/docs/core-components/execution-components/agent-runtime.md): adapter
  composition, dispatch, and provider lifecycle.
- [Harness](@/docs/core-components/execution-components/harness.md): the agent loop, the
  LLM behind it, and observed activity.
