+++
title = "Module Map"
description = "Layer-level ownership map for the workspace crates and the agentty application layers."
weight = 3
+++

<a id="architecture-module-map-introduction"></a> This guide maps the workspace crates
and the `agentty` application layers to their responsibilities so contributors can
quickly choose the correct module when implementing changes.

For file-level detail, run `cargo run -p ag-xtask -- workspace-map`, which writes a
machine-readable workspace summary to `target/agentty/workspace-map.json`, or read the
module docstrings directly.

<!-- more -->

## Workspace Crates

- `crates/ag-forge/`: Shared forge review-request library crate with normalized
  review-request types, GitHub/GitLab remote detection, and the `gh`/`glab` adapters
  behind the `ReviewRequestClient` and `ForgeCommandRunner` boundaries.
- `crates/agentty/`: Main TUI application crate with composition root, application,
  domain, infrastructure, runtime, and UI layers.
- `crates/testty/`: Rust-native TUI end-to-end testing framework with PTY-driven
  semantic assertions and VHS visual capture. Also ships the language-agnostic `testty`
  command-line binary for non-Rust projects.
- `crates/ag-xtask/`: Workspace maintenance commands and automation helpers, including
  the generated workspace-map output.

## Application Layers (`crates/agentty/src/`)

- `main.rs` / `lib.rs`: Composition root — database bootstrap, `App` construction,
  runtime launch, and public module exports.
- `app/`: Orchestration layer. Owns the `App` state, the `AppEvent` reducer, project and
  settings managers, the merge queue, the sync orchestrator, branch publish, focused
  review, and the session module (`app/session/`) with its per-session worker queues and
  workflow steps (`lifecycle`, `turn`, `post_turn`, `merge`, `task`, `worker`). No
  direct process, filesystem, or clock calls — everything external goes through `infra/`
  traits.
- `domain/`: Pure business entities and logic — agent kinds and models, sessions and
  statuses, projects, settings keys, themes, structured questions, typed transcript
  messages, prompt-composer logic, and turn prompt payloads. No I/O.
- `infra/`: External integrations behind traits — SQLite persistence (`infra/db/`
  repositories), git (`GitClient`), filesystem (`FsClient`), tmux, clipboard images,
  version checks, project discovery, file indexing, and the agent stack: provider
  registry, per-provider backends (`infra/agent/`), shared prompt preparation and
  access-root selection (`infra/agent/prompt.rs`), transport channels
  (`infra/channel/`), app-server clients plus shared command and stdio transport helpers
  (`infra/agent/app_server/`), and the structured response protocol
  (`infra/agent/protocol/`).
- `runtime/`: Terminal lifecycle and the event loop — terminal setup, the event-reader
  thread, key dispatch, and one handler per `AppMode` under `runtime/mode/`.
- `ui/`: Rendering — frame composition, mode-to-page routing, pages under `ui/page/`,
  reusable widgets under `ui/component/`, UI state under `ui/state/`, plus markdown,
  diff, layout, and theme helpers. Render caches are owned by the shared
  `RenderCacheStore`.

## Layer Rules

- Workflow and state transitions live in `app/`, not in UI rendering modules.
- Business entities and enums live in `domain/`.
- External side effects live in `infra/` behind mockable traits; see
  [Testability Boundaries](@/docs/architecture/testability-boundaries.md).
- `module.rs` files paired with a `module/` directory stay router-only.
- Change-path guidance for common scenarios lives in
  [Change Recipes](@/docs/architecture/change-recipes.md).
