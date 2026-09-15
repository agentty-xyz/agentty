# ag-session

Frontend-neutral session models and programmatic lifecycle API.

## Boundaries

- Keep TUI state, SQLite rows, Git mechanics, agent workers, and forge clients in host
  adapters behind `SessionBackend`.
- Keep `SessionService` an owned, cloneable capability over a thread-safe backend;
  background coordinators must not borrow a frontend.
- Extend the API with explicit request and result types rather than host-specific
  managers.
- `SessionStatus::can_transition_to()` is the canonical lifecycle graph. Update it and
  its tests instead of duplicating transitions in callers or prose.

## Integration

- Hosts implement `SessionBackend` and construct `SessionService` from a shared backend
  handle. Consumers use the service without borrowing application state.
- Submit coordinator-owned turns through the durable coordinator API rather than the
  live-chat queue. Preserve complete session lookup and complete question-set answers.
- Consult `crates/ag-session/src/service.rs` for request and lifecycle contracts.

## Documentation

Keep `docs/site/content/docs/architecture/runtime-flow.md` aligned with host/service
wiring and `docs/site/content/docs/usage/workflow.md` aligned with lifecycle behavior.
