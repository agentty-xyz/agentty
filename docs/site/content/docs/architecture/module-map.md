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
  prompt templates, provider-neutral channel contracts, the injectable `OneShotClient`
  submission boundary, provider availability probes, and crate-private CLI/app-server
  transport wiring.
- `crates/ag-forge/`: Shared forge review-request library crate with normalized
  review-request and comment-thread types, GitHub/GitLab remote detection, thread
  reply/resolution, and the `gh`/`glab` adapters and project-scoped assigned GitHub
  issue list/detail loading behind the `ReviewRequestClient` and `ForgeCommandRunner`
  boundaries.
- `crates/ag-git/`: Shared git library crate with worktree creation, repository
  metadata, commit/diff/push/pull sync, rebase/conflict handling, and squash-merge
  workflows behind the `GitClient` boundary.
- `crates/ag-harness/`: Application-facing LLM harness crate with the provider-neutral
  `Model` contract, its first Qwen adapter, backend-neutral call instrumentation, and
  GenAI request metrics. Application binaries own OpenTelemetry SDK and OTLP exporter
  setup. Its local `AGENTS.md` records the intended agent loop, tools, session, event,
  and permission boundaries.
- `crates/ag-protocol/`: Shared structured response protocol library crate with
  transport-neutral response models, schema generation, parser diagnostics, protocol
  prompt envelopes, repair prompts, review-comment outcomes, and turn prompt payload
  helpers.
- `crates/ag-session/`: Frontend-neutral session library with stable identity,
  lifecycle, review-link, and transcript models; complete session aggregates; and the
  object-safe `SessionBackend` port exposed through the owned, cloneable
  `SessionService` for creation, lookup, messaging, structured question answers, durable
  coordinator submissions, cancellation, merge, and review-request workflows.
- `crates/ag-tui-text/`: Shared Ratatui text-rendering library crate with Markdown
  parsing/styling, forge HTML normalization, bounded mermaid-to-terminal diagram
  rendering, and terminal-width wrapping/truncation helpers. Host applications inject
  semantic palette and cache version settings at the render boundary.
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
  settings persistence manager, the merge queue, the project sync orchestrator, durable
  campaign planning, managed-worker capability routing, the multi-session orchestration
  coordinator, branch publish, review, typed prompt workflow requests and outcomes, the
  `session_api.rs` adapter for `ag-session`, the bounded `session_runtime.rs` command
  actor, and the session module (`app/session/`) with its per-session worker queues and
  workflow steps (`lifecycle`, `turn`, `post_turn`, `merge`, `task`, `worker`). Prompt
  composers, slash-menu state, and mode navigation remain presentation-owned. No direct
  process, filesystem, or clock calls — everything external goes through `infra/`
  traits.
- `domain/`: Pure Agentty-specific business entities and logic — render/runtime session
  snapshots, projects, settings keys, themes, structured questions, explicit
  transient-message slots and lifecycles, prompt-composer logic, the shared `InputState`
  command and undo/redo model, stable input-revision and character-offset identities
  used to bind prompt attachments to exact placeholder occurrences and history states,
  session action-eligibility policies, orchestration and orchestration-task lifecycle
  states with their transition graph, fuzzy file-entry ranking shared by runtime
  selection and UI suggestions, personality definition parsing and prompt
  fingerprinting, and thin re-export modules for `ag-agent` provider models,
  `ag-session` identity/status/transcript models, and shared protocol question and turn
  prompt payloads. No I/O.
- `infra/`: External integrations behind traits — Agentty data-root resolution, SQLite
  persistence (`infra/db/` repositories), git (`GitClient`, backed by `ag-git`),
  filesystem (`FsClient`), the session-worktree-only personality catalog, tmux,
  clipboard images, version checks, project discovery, and file indexing. Clipboard
  image capture delegates host clipboard reads to `ag-clipboard`, then owns temp-file
  persistence and attachment metadata. Agentty imports the curated `ag-agent` crate-root
  API; provider registry, router, parser, and transport internals stay private to
  `crates/ag-agent/`.
- `runtime/`: Terminal lifecycle and the event loop — terminal setup, the event-reader
  thread, key dispatch, mode-focused handlers under `runtime/mode/`, and shared handlers
  for common interactions such as issue/review detail navigation, session-output
  metrics, transcript scrolling, `KeyEvent` mapping to domain input commands, and
  session review-comment navigation, address/deny marking, and batch submission. Runtime
  owns `PresentationState`, including the shared `RenderCacheStore` used by input
  metrics and frame rendering.
- `presentation.rs` and `presentation/`: Frontend-neutral interaction state shared by
  runtime input and UI output. They expose mode, help-action, prompt, settings-screen
  actions, editor, scroll, viewport, semantic list-selection contracts, and one coherent
  `FrameTime` value per render pass without importing Ratatui or `ui/` formatting.
  `presentation/review_comment.rs` owns review comment group ordering and headings while
  preserving forge-thread selection and batch actions across grouped snapshot refreshes.
  `presentation/settings.rs` owns settings row selection, selectors,
  launch-configuration editing through the shared `InputState`, and render-ready
  settings snapshots; it returns typed persistence operations to `app/setting.rs`.
- `ui/`: Rendering — frame composition, mode-to-page routing, pages under `ui/page/`,
  reusable widgets under `ui/component/`, application-to-frame projection in
  `ui/app_render.rs`, Agentty theme adapters for `ag-tui-text`, plus diff, layout,
  review-comment formatting, the split session review-comment page, and theme helpers.
  `ui/session_output_assembly.rs` owns the pure transcript-to-display-line projection;
  the `SessionOutput` component retains layout caching, scrollbar metrics, loader
  effects, and Ratatui painting.

## Layer Rules

- Workflow and state transitions live in `app/`, not in UI rendering modules.
- `App` does not render terminal frames or own concrete render caches; runtime passes
  its presentation cache into the UI projection boundary.
- `App::view_snapshot()` creates the immutable borrowed application view consumed by
  frontends. `ui/app_render.rs` receives that snapshot plus runtime-owned Ratatui state
  and does not access the concrete `App`, services, or managers directly. The snapshot
  resolves the injected clock once into `FrameTime`, including Unix seconds,
  milliseconds, and the clock-provided UTC offset used by deterministic timers, loaders,
  and activity-day projections. Fixed clocks own both their timestamp and offset, so
  render projections do not depend on the host timezone.
- Session activity persistence stores timestamps supplied by the injected `Clock`.
  Session loading retrieves those immutable timestamps and applies the clock-provided
  offset for each event before aggregating local-day counts; SQLite does not read the
  host clock or timezone for this projection.
- Application managers retain semantic selected-row indexes through
  `domain::selection::SelectionState`; runtime owns Ratatui table viewport state and
  synchronizes selection at the frame projection boundary.
- `app/` must not import runtime mode handlers. Shared interaction calculations belong
  in `domain/` or `presentation.rs`, while application task registries belong in `app/`.
- Runtime converts presentation-owned prompt state into typed app requests, then applies
  returned navigation and composer effects. `app/` must not inspect or mutate `AppMode`.
- Business entities and enums live in `domain/`.
- External side effects live in `infra/` behind mockable traits; see
  [Testability Boundaries](@/docs/architecture/testability-boundaries.md).
- `module.rs` files paired with a `module/` directory stay router-only.
- Change-path guidance for common scenarios lives in
  [Change Recipes](@/docs/architecture/change-recipes.md).
