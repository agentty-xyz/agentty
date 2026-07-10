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

- `crates/ag-clipboard/`: Read-only clipboard support crate with the narrow text,
  file-list, and RGBA image read surface used by prompt image capture. Platform backends
  own macOS pasteboard access, X11 selection reads, Wayland `wl-paste` reads, and
  unsupported-backend reporting.
- `crates/ag-agent/`: Shared agent backend library crate with provider model metadata,
  prompt templates, provider-neutral channel contracts, one-shot submission helpers,
  provider availability probes, and crate-private CLI/app-server transport wiring.
- `crates/ag-forge/`: Shared forge review-request library crate with normalized
  review-request types, GitHub/GitLab remote detection, and the `gh`/`glab` adapters
  behind the `ReviewRequestClient` and `ForgeCommandRunner` boundaries.
- `crates/ag-git/`: Shared git library crate with worktree creation, repository
  metadata, commit/diff/push/pull sync, rebase/conflict handling, and squash-merge
  workflows behind the `GitClient` boundary.
- `crates/ag-protocol/`: Shared structured response protocol library crate with
  transport-neutral response models, schema generation, parser diagnostics, protocol
  prompt envelopes, repair prompts, and turn prompt payload helpers.
- `crates/ag-tui-text/`: Shared Ratatui text-rendering library crate with markdown
  parsing/styling, bounded mermaid-to-terminal diagram rendering, and terminal-width
  wrapping/truncation helpers. Host applications inject semantic palette and cache
  version settings at the render boundary.
- `crates/agentty/`: Main TUI application crate with composition root, application,
  domain, infrastructure, runtime, and UI layers.
- `crates/testty/`: Rust-native TUI end-to-end testing framework with PTY-driven
  semantic assertions and VHS visual capture. Also ships the language-agnostic `testty`
  command-line binary for non-Rust projects.
- `crates/ag-xtask/`: Workspace maintenance commands, including the SQL migration
  numbering check.

## Application Layers (`crates/agentty/src/`)

- `main.rs` / `lib.rs`: Composition root — database bootstrap, `App` construction,
  runtime launch, and public module exports.
- `app/`: Orchestration layer. Owns the `App` state, the `AppEvent` reducer, project and
  settings managers, the merge queue, the sync orchestrator, branch publish, focused
  review, and the session module (`app/session/`) with its per-session worker queues and
  workflow steps (`lifecycle`, `turn`, `post_turn`, `merge`, `task`, `worker`). No
  direct process, filesystem, or clock calls — everything external goes through `infra/`
  traits.
- `domain/`: Pure business entities and logic — sessions and statuses, projects,
  settings keys, themes, structured questions, typed transcript messages,
  prompt-composer logic, and thin re-export modules for `ag-agent` provider models plus
  shared protocol question and turn prompt payloads. No I/O.
- `infra/`: External integrations behind traits — Agentty data-root resolution, SQLite
  persistence (`infra/db/` repositories), git (`GitClient`, backed by `ag-git`),
  filesystem (`FsClient`), tmux, clipboard images, version checks, project discovery,
  and file indexing. Clipboard image capture delegates host clipboard reads to
  `ag-clipboard`, then owns temp-file persistence and attachment metadata. Agentty
  imports the curated `ag-agent` crate-root API; provider registry, router, parser, and
  transport internals stay private to `crates/ag-agent/`.
- `presentation/`: Shared presentation contracts — `AppMode`, prompt/help state,
  renderer-neutral table selection/offset state, semantic theme styling, icons, and
  user-visible status-message formatting. This layer lets app orchestration, runtime
  input, and UI rendering share presentation state without making `app/` depend on
  `runtime/` or `ui/`.
- `runtime/`: Terminal lifecycle and the event loop — terminal setup, the event-reader
  thread, key dispatch, one handler per `AppMode` under `runtime/mode/`, and shared mode
  helpers for session-output metrics. The runtime owns the active render-cache store and
  passes it to frame rendering and metric helpers.
- `ui/`: Rendering — frame composition, mode-to-page routing, pages under `ui/page/`,
  reusable widgets under `ui/component/`, Agentty adapters for `ag-tui-text`, plus diff,
  layout, and review-comment formatting. `App` exposes a borrowed render projection;
  `ui::draw()` adapts renderer-neutral table state into Ratatui `TableState` values and
  converts the projection into a `RenderContext` without moving rendering ownership into
  app orchestration. Render-cache and Ratatui widget-state types remain UI-owned.

## Layer Rules

- Workflow and state transitions live in `app/`, not in UI rendering modules.
- `app/` may depend on `domain/`, `infra/`, and `presentation/`, but not on `runtime/`
  or `ui/`.
- `runtime/` coordinates input and drawing; `ui/` consumes app render projections and
  shared `presentation/` contracts.
- Business entities and enums live in `domain/`.
- External side effects live in `infra/` behind mockable traits; see
  [Testability Boundaries](@/docs/architecture/testability-boundaries.md).
- `module.rs` files paired with a `module/` directory stay router-only.
- Change-path guidance for common scenarios lives in
  [Change Recipes](@/docs/architecture/change-recipes.md).
