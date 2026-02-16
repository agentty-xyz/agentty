# App Module

Application-layer workflows and orchestration.

## Overview
- The app layer coordinates session lifecycle, prompt/reply submission, async worker execution, persistence, and UI state refresh.
- Mode handlers prefer enqueue-first behavior for long-running work so the UI remains responsive.

## Design
- State model:
  - `Session` is a render-friendly data snapshot.
  - `SessionHandles` stores shared runtime channels used by background tasks.
  - `SessionState` owns both collections and performs handle-to-snapshot sync.
- Event model:
  - `AppEvent` is the internal bus between background workflows and the runtime loop.
  - `apply_app_events()` is the reducer for app-side async mutations.
  - Session configuration/history mutations (`agent/model`, `permission_mode`, history reset) are persisted first, then reduced via events.
  - Foreground workflows should emit via `dispatch_app_event()` instead of mutating reducer state directly.
- Execution model:
  - Work is serialized per session through worker queues.
  - PR and merge workflows run in background tasks and report progress through events and persisted status/output.
- Refresh model:
  - List reloads are event-driven (`RefreshSessions`) at lifecycle boundaries.
  - A low-frequency metadata poll remains as a safety fallback.
- Recovery model:
  - Operation state is persisted so interrupted work can be reconciled on startup.

## Directory Index
- [mod.rs](mod.rs) - Shared app state and module wiring.
- [pr.rs](pr.rs) - Pull request workflow orchestration.
- [project.rs](project.rs) - Project discovery and switching logic.
- [session.rs](session.rs) - Session lifecycle, persistence, and git worktree flow.
- [task.rs](task.rs) - Background task spawning and output handling.
- [title.rs](title.rs) - Session title summarization helpers.
- [worker.rs](worker.rs) - Per-session worker queue and operation lifecycle orchestration.
