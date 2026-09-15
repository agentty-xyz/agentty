+++
title = "Execution"
description = "How run workers, runtime contracts, harnesses, and LLMs fit together."
weight = 1
+++

Execution follows four layers: **Run Worker → Agent Runtime → Harness → LLM**.

## Run Worker

`ag-worker` schedules runs and coordinates heartbeats, cancellation, completion, and
restart recovery. Hosts supply workflow policy and persistence; the worker has no TUI,
Git, or database implementation dependency.

## Agent Runtime

`ag-runtime` defines the shared contract for submitting turns and receiving events,
results, and errors. `ag-agent` implements that contract for external CLI and app-server
harnesses, owning transport details and provider resource cleanup.

## Harness

A harness runs the agent loop: it combines context, calls models, executes tools, and
decides when to continue or finish. External agent tools provide this loop today.
`ag-harness` provides a standalone Rust implementation; connecting it through
`ag-runtime` remains separate work.

## LLM

The LLM is the model invoked by the harness for reasoning and generation. It does not
schedule Agentty runs or execute tools itself. `ag-harness` separates model access
behind its `ModelClient` contract; external harnesses manage their own model
integrations.

## Supporting Boundaries

`ag-session` owns session models and the built-in agent/model catalog. `ag-store`
provides SQLite persistence, including the worker's operation records. Agentty wires
these components together and supplies application workflows.

See [Module Map](@/docs/architecture/module-map.md) for ownership details and
[Runtime Flow](@/docs/architecture/runtime-flow.md) for orchestration.
