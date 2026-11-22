# ag-store

Reusable persistence contracts, SQLite adapters, and embedded migrations.

## Boundaries

- Keep Agentty filesystem layout, TUI state, Git workflows, and rendering out of this
  crate.
- Use shared models from `ag-session` and transport-independent settings from
  `ag-contracts`. Implement the worker-owned `OperationRepository` contract from
  `ag-worker`; keep `ag-agent` provider transports and `agentty` out of persistence.
- Expose repository mocks through `test-utils` when dependents need deterministic
  persistence tests.

## Integration

- Hosts choose database paths and open `Database`, which applies embedded migrations and
  exposes `AppRepositories`. Inject narrow repository traits into workflows.
- Inject `TimestampSource` when the host controls time. Use in-memory SQLite and
  injected timestamps for deterministic repository tests.
- The `test-utils` constructors load a migrated-schema snapshot cached in the temporary
  directory and keyed by every migration checksum; keep migration and constructor tests
  on `Database::open_in_memory`, which replays migrations.
- Consult `crates/ag-store/src/connection.rs` for initialization and
  `crates/ag-store/tests/contract/repository.rs` for consumer examples.

## SQLite Invariants

- Use SQLx directly, without an ORM, and prefer checked query macros. Keep
  `crates/ag-store/.sqlx/` metadata current for offline builds.
- Keep migrations embedded and connection setup configured for foreign keys and WAL.
- Other processes and pools share the database file. Begin every transaction that may
  write with `begin_write` and an operation label; it takes the write lock up front, so
  add no no-op writes to take it. Keep plain `begin` for read-only snapshots.
- Use singular table names, and end timestamp columns in `_at`.
- Name every boolean column `is_` or `has_` and constrain it with
  `CHECK (<column> IN (0, 1))`; the schema contract test finds booleans only through
  that constraint.

## Documentation

Follow `skills/regenerate-sqlx-metadata/SKILL.md` for offline-query metadata
regeneration and `docs/site/content/docs/architecture/change-recipes.md` for schema
changes. Update `docs/site/content/docs/architecture/testability-boundaries.md` when
repository or clock contracts change.
