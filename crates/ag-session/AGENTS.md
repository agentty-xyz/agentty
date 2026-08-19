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
