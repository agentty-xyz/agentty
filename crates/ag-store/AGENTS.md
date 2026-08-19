# ag-store

Reusable persistence contracts, SQLite adapters, and embedded migrations.

## Boundaries

- Keep Agentty filesystem layout, TUI state, Git workflows, and rendering out of this
  crate.
- Depend on shared models through `ag-session` and provider metadata through `ag-agent`;
  never depend on `agentty`.
- Expose repository mocks through `test-utils` when dependents need deterministic
  persistence tests.

## SQLite Invariants

- Use SQLx directly, without an ORM, and prefer checked query macros. Keep `.sqlx/`
  metadata current for offline builds.
- Keep migrations embedded and connection setup configured for foreign keys and WAL.
- Never edit an existing migration. Add a numbered
  `crates/ag-store/migrations/NNN_description.sql` file and run the migration check.
- Use singular `snake_case` table names and `snake_case` columns. New foreign keys use
  `<table>_id`, booleans use `is_` or `has_`, and timestamps end in `_at`.
- Translate `sqlx::Error` into the crate's typed error surface.

Use in-memory SQLite and injected timestamp sources for deterministic repository tests.
