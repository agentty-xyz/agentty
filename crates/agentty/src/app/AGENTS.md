# Application Layer

Coordinates session and project workflows, persistence, background work, and
presentation-state refreshes.

## Invariants

- One foreground task owns reducer state. Background work emits `AppEvent` values, and
  programmatic callers use the bounded session-runtime command channel; neither mutates
  `App` state directly.
- Keep state transitions deterministic: derive an ordered state/effect plan before
  executing external effects.
- Serialize session commands through post-processing. Enqueue long-running commands so
  the terminal loop remains responsive.
- Persist recoverable operation state and reconcile interrupted work at startup.
- Do not probe the host filesystem or invoke processes directly. Route discovery,
  metadata, path checks, clocks, Git, and other external work through injected infra
  traits.

## Model Work Integration

- Inject `ag-worker`'s `RunClient` for utility prompts. Confine raw runtime adapter
  construction to application composition; runtime execution belongs to the worker.
- Await utility children directly through `RunClient`, without queuing them behind their
  waiting parent; the worker bounds utility concurrency.
- Capture `RunScope` with `scoped_client` before spawning background work. Preserve
  parent operation, session/project ownership, and cancellation across nested calls;
  pass request permissions and provider-call budgets through unchanged.
- Keep `crates/agentty/src/execution_boundary_test.rs` enforcing the boundary when
  adding model actions. Test new workflows through the worker submission contract.

## Documentation

- Update `docs/site/content/docs/usage/workflow.md` for lifecycle behavior and
  `docs/site/content/docs/usage/keybindings.md` for visible actions.
- Update `docs/site/content/docs/architecture/runtime-flow.md` when orchestration,
  reducer, worker, or channel flow changes.
- Keep `docs/site/content/docs/core-components/execution.md` aligned with changes to
  model execution ownership.
