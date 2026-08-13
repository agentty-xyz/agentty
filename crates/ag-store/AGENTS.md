# Store Crate

Owns reusable repository contracts, SQLite adapters, and embedded migrations for Agentty
session persistence.

## Boundaries

- `lib.rs` exposes repository traits, persisted row and request types, `Database`, and
  `AppRepositories`.
- `connection.rs` owns SQLite pool configuration and runs migrations from `migrations/`.
- Repository modules own SQL for their corresponding singular tables and aggregates.
- `timestamp.rs` defines the narrow timestamp source injected into write adapters.
- Keep Agentty filesystem layout, TUI state, Git workflows, and rendering concerns out
  of this crate.
- Depend on shared models through `ag-session` and provider metadata through `ag-agent`;
  never depend on `agentty`.

## Tests

- Keep repository and migration tests local to this crate.
- Use in-memory SQLite pools for query behavior and injected timestamp sources for
  deterministic writes.
- Enable the `test-utils` feature when a dependent crate needs generated repository
  mocks.
