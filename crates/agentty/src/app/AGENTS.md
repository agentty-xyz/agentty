# App Module

Application-layer workflows and orchestration.

## Overview

- The app layer coordinates session lifecycle, prompt/reply submission, async worker
  execution, persistence, and UI state refresh.
- Mode handlers prefer enqueue-first behavior for long-running work so the UI remains
  responsive.

## Design

- Composition model:
  - `App` is a facade/orchestrator.
  - `SessionRuntime` owns `SessionManager` plus a bounded command mailbox.
  - `SessionRuntimeHandle` implements the frontend-neutral `ag-session` `SessionBackend`
    port; `session_api.rs` executes accepted commands against the foreground-owned
    runtime and host services.
  - `SessionManager` inside the runtime coordinates session snapshots, workflow state,
    and session worker queues.
  - `ProjectManager` owns project list, active project context, and git status tracking
    state.
  - `AppServices` holds shared dependencies (`Database`, base path, app-event sender).
- Session state model:
  - `Session` is a render-friendly data snapshot.
  - `SessionRuntimeState` owns `SessionHandles`, which store shared runtime channels
    used by background tasks.
  - `SessionState` owns persisted/render-friendly projections and performs
    handle-to-snapshot sync before render.
- Event model:
  - `AppEvent` is the internal bus between background workflows and the runtime loop.
  - `SessionRuntimeCommand` carries programmatic session requests through a bounded
    channel and returns each result through a one-shot response channel.
  - Runtime handles observe a reference-counted foreground-consumer signal, rejecting
    requests while no command consumer is registered and abandoning pending waits when
    the final consumer exits.
  - `apply_app_events()` coalesces app-side async mutations; each batch first produces a
    deterministic state/effect plan, then the app executes the ordered effects.
  - The foreground event loop selects between `AppEvent` values and session-runtime
    commands so both mutate reducer-owned state on the same task.
  - Programmatic creation reloads the active-project session snapshot before
    acknowledging, so unrelated queued app events cannot hide the new session from a
    following command. A transient reload failure does not turn durable creation into an
    ambiguous error; the API returns the created id and schedules another session
    refresh.
  - Programmatic question answers claim the persisted question set before enqueueing the
    continuation directly on the per-session worker, bypassing the in-memory chat queue,
    and restore it when enqueueing fails.
  - Background tasks and manager workflows emit events through `AppServices`.
  - Foreground `App` wrappers process queued events to keep reducer-driven state
    coherent.
- Execution model:
  - Work is serialized per session through worker queues.
  - Merge workflows run in background tasks and report progress through events and
    persisted status/output.
- Refresh model:
  - List reloads are event-driven (`RefreshSessions`) at lifecycle boundaries.
  - API creation forces a direct active-project reload before returning its new session
    id and queues a retry when that registration attempt does not load the durable row.
  - A low-frequency metadata poll remains as a safety fallback.
- Recovery model:
  - Operation state is persisted so interrupted work can be reconciled on startup.
- Boundary model:
  - Keep production filesystem discovery and path probes out of `app/`.
  - Route directory walking, `exists` or `is_dir` checks for external paths, and similar
    host-filesystem lookups through infra traits instead of calling `std::fs`,
    `tokio::fs`, or `Path` helpers directly from app orchestration.

## Docs Sync

When app orchestration or session lifecycle behavior changes, update:

- `docs/site/content/docs/usage/workflow.md` — statuses, transitions, question flow, and
  slash-command behavior.
- `docs/site/content/docs/usage/keybindings.md` — user-visible actions available per
  mode/state.
- `docs/site/content/docs/architecture/runtime-flow.md` — app orchestration and
  worker/channel runtime flow.

## Entry Points

- `core.rs` owns the main `App` facade and reducer wiring.
- `session_runtime.rs` owns the bounded actor mailbox and cloneable control handle.
- `session_api.rs` owns actor-command execution, complete aggregate loading, and the
  programmatic API adapter.
- `session.rs` and `session/` own session lifecycle and worker orchestration.
- `project.rs`, `setting.rs`, and `tab.rs` own project, settings, and top-level
  navigation concerns.
- `task.rs` and `merge_queue.rs` own detached workflows and queue orchestration.
