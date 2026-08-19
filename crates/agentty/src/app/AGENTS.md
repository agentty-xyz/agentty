# Application Layer

Coordinates session and project workflows, persistence, background work, and
presentation-state refreshes.

## Invariants

- One foreground task owns reducer state. Background work emits `AppEvent` values, and
  programmatic callers use the bounded session-runtime command channel; neither mutates
  `App` state directly.
- Keep state transitions deterministic: derive an ordered state/effect plan before
  executing external effects.
- Serialize work per session. Enqueue long-running work so the terminal loop remains
  responsive.
- Persist recoverable operation state and reconcile interrupted work at startup.
- Do not probe the host filesystem or invoke processes directly. Route discovery,
  metadata, path checks, clocks, Git, and other external work through injected infra
  traits.

## Documentation

- Update `docs/site/content/docs/usage/workflow.md` for lifecycle behavior and
  `docs/site/content/docs/usage/keybindings.md` for visible actions.
- Update `docs/site/content/docs/architecture/runtime-flow.md` when orchestration,
  reducer, worker, or channel flow changes.
