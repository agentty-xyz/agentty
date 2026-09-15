# ag-store

Reusable persistence contracts, SQLite adapters, and embedded migrations.

## Boundaries

- Keep Agentty filesystem layout, TUI state, Git workflows, and rendering out of this
  crate.
- Use shared models from `ag-session` and transport-independent settings from
  `ag-runtime`. Implement the worker-owned `OperationRepository` contract from
  `ag-worker`; keep `ag-agent` provider transports and `agentty` out of persistence.
- Expose repository mocks through `test-utils` when dependents need deterministic
  persistence tests.

## Integration

- Hosts choose database paths and open `Database`, which applies embedded migrations and
  exposes `AppRepositories`. Inject narrow repository traits into workflows.
- Inject `TimestampSource` when the host controls time. Use in-memory SQLite and
  injected timestamps for deterministic repository tests.
- Consult `crates/ag-store/src/connection.rs` for initialization and
  `crates/ag-store/tests/repository.rs` for consumer examples.

## SQLite Invariants

- Use SQLx directly, without an ORM, and prefer checked query macros. Keep
  `crates/ag-store/.sqlx/` metadata current for offline builds.
- Keep migrations embedded and connection setup configured for foreign keys and WAL.
- Never edit an existing migration. Add a numbered
  `crates/ag-store/migrations/NNN_description.sql` file and run the migration check.
- Use singular `snake_case` table names and `snake_case` columns. New foreign keys use
  `<table>_id`, booleans use `is_` or `has_`, and timestamps end in `_at`.
- Translate `sqlx::Error` into the crate's typed error surface.

## Documentation

Follow `CONTRIBUTING.md` for offline-query metadata regeneration and
`docs/site/content/docs/architecture/change-recipes.md` for schema changes. Update
`docs/site/content/docs/architecture/testability-boundaries.md` when repository or clock
contracts change.
