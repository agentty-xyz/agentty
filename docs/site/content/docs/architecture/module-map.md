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

- `crates/ag-clipboard/`: Read-only clipboard support crate with the narrow text,
  file-list, and RGBA image read surface used by prompt image capture. Platform backends
  own macOS pasteboard access, X11 selection reads, Wayland `wl-paste` reads, and
  unsupported-backend reporting.
- `crates/ag-agent/`: Shared agent backend library crate with provider model metadata,
  prompt templates, CLI and app-server transports, provider-neutral channel contracts,
  app-server routing, and compatibility re-exports for the moved Agentty agent APIs.
- `crates/ag-forge/`: Shared forge review-request library crate with normalized
  review-request types, GitHub/GitLab remote detection, and the `gh`/`glab` adapters
  behind the `ReviewRequestClient` and `ForgeCommandRunner` boundaries.
- `crates/ag-git/`: Shared git library crate with worktree creation, repository
  metadata, commit/diff/push/pull sync, rebase/conflict handling, and squash-merge
  workflows behind the `GitClient` boundary.
- `crates/ag-protocol/`: Shared structured response protocol library crate with
  transport-neutral response models, schema generation, parser diagnostics, protocol
  prompt envelopes, repair prompts, and turn prompt payload helpers.
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
- `domain/`: Pure business entities and logic — sessions and statuses, projects,
  settings keys, themes, structured questions, typed transcript messages,
  prompt-composer logic, and compatibility re-exports for `ag-agent` provider models
  plus shared protocol question and turn prompt payloads. No I/O.
- `infra/`: External integrations behind traits — SQLite persistence (`infra/db/`
  repositories), git (`GitClient`, backed by `ag-git`), filesystem (`FsClient`), tmux,
  clipboard images, version checks, project discovery, file indexing, and the agent
  stack. Clipboard image capture delegates host clipboard reads to `ag-clipboard`, then
  owns temp-file persistence and attachment metadata. Agent backend, channel,
  app-server, and provider registry APIs are owned by `crates/ag-agent/` and re-exported
  through `infra/agent`, `infra/channel`, `infra/app_server`, `infra/app_server_router`,
  and `infra/app_server_transport` for Agentty callers.
- `runtime/`: Terminal lifecycle and the event loop — terminal setup, the event-reader
  thread, key dispatch, one handler per `AppMode` under `runtime/mode/`, and shared mode
  helpers for session-output metrics.
- `ui/`: Rendering — frame composition, mode-to-page routing, pages under `ui/page/`,
  reusable widgets under `ui/component/`, UI state under `ui/state/`, plus markdown,
  diff, layout, review-comment formatting, and theme helpers. Render caches are owned by
  the shared `RenderCacheStore`.

## Layer Rules

- Workflow and state transitions live in `app/`, not in UI rendering modules.
- Business entities and enums live in `domain/`.
- External side effects live in `infra/` behind mockable traits; see
  [Testability Boundaries](@/docs/architecture/testability-boundaries.md).
- `module.rs` files paired with a `module/` directory stay router-only.
- Change-path guidance for common scenarios lives in
  [Change Recipes](@/docs/architecture/change-recipes.md).
