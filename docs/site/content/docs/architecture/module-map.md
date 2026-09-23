+++
title = "Module Map"
description = "Layer-level ownership map for the workspace crates and the agentty application layers."
weight = 3
+++

<a id="architecture-module-map-introduction"></a> This guide maps the workspace crates
and the `agentty` application layers to their responsibilities so contributors can
quickly choose the correct module when implementing changes.

For file-level detail, read the module docstrings directly.

<!-- more -->

## Workspace Crates

| Crate              | Responsibility                                                   |
| ------------------ | ---------------------------------------------------------------- |
| `ag-clipboard`     | Host clipboard reads                                             |
| `ag-contracts`     | Transport-independent execution types and interfaces             |
| `ag-runtime`       | Adapter composition and dispatch                                 |
| `ag-worker`        | Scheduling, cancellation, heartbeats, recovery, execution policy |
| `ag-agent`         | External CLI and app-server adapters, policy enforcement         |
| `ag-forge`         | GitHub/GitLab review requests and comments                       |
| `ag-git`           | Worktrees, diffs, sync, rebase, merge                            |
| `ag-harness`       | Standalone model loop, tools, durable sessions                   |
| `ag-harness-cli`   | Companion harness CLI                                            |
| `ag-orchestration` | Campaign planning, verification, integration                     |
| `ag-protocol`      | Response schemas, parsing, prompt envelopes                      |
| `ag-session`       | Session models, policies, catalog, lifecycle API                 |
| `ag-store`         | Persistence contracts, SQLite adapters, migrations               |
| `ag-tui-text`      | Markdown, forge HTML, terminal diagrams and text layout          |
| `agentty`          | Application composition and TUI                                  |
| `testty`           | PTY assertions and visual recordings                             |
| `ag-xtask`         | Workspace maintenance checks                                     |

All crates live under `crates/`. See
[`ag-harness` Design](@/docs/architecture/ag-harness-design.md) and
[Orchestrator Design](@/docs/architecture/orchestrator.md) for their component
contracts.

## Application Layers (`crates/agentty/src/`)

| Layer                | Owns                                                            |
| -------------------- | --------------------------------------------------------------- |
| `main.rs` / `lib.rs` | Bootstrap and composition                                       |
| `app/`               | Workflows, state transitions, event reduction, service adapters |
| `domain/`            | Pure Agentty entities and interaction policies                  |
| `infra/`             | External integrations behind injectable traits                  |
| `runtime/`           | Terminal lifecycle, event loop, input dispatch                  |
| `presentation/`      | Shared interaction, selection, editor, and viewport state       |
| `ui/`                | Snapshot projection, layout, and rendering                      |

## Layer Rules

- Keep workflow changes in `app/` and external effects behind `infra/` traits.
- Keep frontend-neutral session models in `ag-session` and persistence in `ag-store`.
- Runtime converts presentation state into typed app requests and applies returned
  navigation effects. `app/` does not import runtime handlers or mutate `AppMode`.
- `App::view_snapshot()` supplies an immutable frontend view. UI projection cannot
  access concrete application services; runtime owns render caches and table viewports.
- Resolve the injected clock once per frame. Persist activity timestamps explicitly and
  apply the clock's UTC offset when grouping events, avoiding host-timezone dependence.
- Share semantic interaction calculations in `domain/` or `presentation/`; keep these
  independent of Ratatui rendering.
- Keep paired `module.rs` routers free of implementation details.

For example, resource accounting puts host sampling in `infra/`, scheduling and cache
invalidation in `app/`, totals in `domain/`, and display formatting in `ui/`.

See [Testability Boundaries](@/docs/architecture/testability-boundaries.md) for external
ports and [Change Recipes](@/docs/architecture/change-recipes.md) for contribution
paths.

## Worker-owned model execution

Application workflows submit session turns through `SessionRunClient` and isolated
utilities through `RunClient`. Only `ag-worker` depends on `ag-runtime`; only
`ag-runtime` depends on `ag-agent`. Shared execution contracts live in `ag-contracts`.
The worker owns runtime lifecycle and cancellation; applications configure workers
rather than construct adapters.

See [Execution](@/docs/core-components/execution.md) for the execution contract.
