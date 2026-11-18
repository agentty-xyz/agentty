+++
title = "Module Map"
description = "Layer-level ownership map for the workspace crates and the agentty application layers."
weight = 3
+++

<a id="architecture-module-map-introduction"></a> This guide maps the workspace crates
and the `agentty` application layers to their responsibilities so contributors can
quickly choose the correct module when implementing changes. For file-level detail, read
the module docstrings directly.

<!-- more -->

## Workspace Crates

| Crate            | Responsibility                                                   |
| ---------------- | ---------------------------------------------------------------- |
| `ag-clipboard`   | Host clipboard reads                                             |
| `ag-contracts`   | Transport-independent execution types and interfaces             |
| `ag-runtime`     | Adapter composition and dispatch                                 |
| `ag-scheduler`   | Shared session admission                                         |
| `ag-worker`      | Scheduling, cancellation, heartbeats, recovery, execution policy |
| `ag-agent`       | CLI, app-server, and native harness adapters; policy enforcement |
| `ag-forge`       | GitHub/GitLab review requests and comments                       |
| `ag-git`         | Worktrees, captured diff parsing, sync, rebase, merge            |
| `ag-harness`     | Native model loop, tools, durable sessions                       |
| `ag-harness-cli` | Companion harness CLI                                            |
| `ag-protocol`    | Response schemas, parsing, prompt envelopes                      |
| `ag-router`      | Structured chat routing and provider transports                  |
| `ag-session`     | Session models, policies, catalog, lifecycle API                 |
| `ag-store`       | Persistence contracts, SQLite adapters, migrations               |
| `ag-telemetry`   | Content-free spans, async trace context, and opt-in OTLP export  |
| `ag-tui-text`    | Markdown, forge HTML, terminal diagrams and text layout          |
| `agentty`        | Application composition and TUI                                  |
| `testty`         | PTY assertions and visual recordings                             |
| `ag-release`     | Release planning, archives, and installers                       |
| `ag-xtask`       | Workspace maintenance checks                                     |

All crates live under `crates/`. See
[`ag-harness` Design](@/docs/architecture/ag-harness-design.md) for its component
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

- Keep workflow changes and shared command admission in `app/`, with external effects
  behind `infra/` traits. The command gateway owns admission; the worker owns execution
  ordering and settlement.
- Keep frontend-neutral session, task, Env, and workflow role contracts in `ag-session`
  and persistence in `ag-store`. Task attempts project execution state from worker-owned
  operations; task completion is explicit outcome acceptance.
- `ag-session` owns portable agent definitions and bundled defaults; the injected
  infrastructure catalog overlays repository definitions. Execution consumes captured
  definitions independently of their IDs.
- [Execution graph](@/docs/architecture/execution-graphs.md) ownership is split:
  `ag-protocol` owns model-authored proposals, `ag-session` pure validation, resolution,
  and result bindings, `ag-store` versions, evidence, and atomic admission, and
  `ag-worker` dispatch and resource leases. Agentty owns proposal approval, explicit
  launch, wakeups, and adapters into session workers; focused review owns its own graph
  construction.
- Runtime converts presentation state into typed app requests and applies returned
  navigation effects. `app/` does not import runtime handlers or mutate `AppMode`.
- `App::view_snapshot()` supplies an immutable frontend view. UI projection cannot
  access concrete application services; runtime owns render caches and table viewports.
- Resolve the injected clock once per frame. Persist activity timestamps explicitly and
  apply the clock's UTC offset when grouping events, avoiding host-timezone dependence.
- Share semantic interaction calculations in `domain/` or `presentation/`; keep these
  independent of Ratatui rendering.

For example, resource accounting puts host sampling in `infra/`, scheduling and cache
invalidation in `app/`, totals in `domain/`, and display formatting in `ui/`.

See [Testability Boundaries](@/docs/architecture/testability-boundaries.md) for external
ports and [Change Recipes](@/docs/architecture/change-recipes.md) for contribution
paths.

## Worker-owned model execution

Application workflows submit session turns through `SessionRunClient` and isolated
utilities through `RunClient`; applications configure workers rather than construct
adapters. See
[Execution](@/docs/core-components/execution-components/execution-boundary.md) for the
dependency rule and execution contract.
