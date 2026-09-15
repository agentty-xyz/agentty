# ag-harness

Provider-neutral structured model turns, durable SQLite sessions, and bounded repository
tools, independent of Agentty UI and orchestration.

## Boundaries

- `Model` is the object-safe application boundary; `ModelClient` owns the common request
  lifecycle, telemetry, and structured-output validation.
- Provider modules own configuration and capability policy. API-family modules own
  shared authentication, translation, and wire handling; keep their runtime types
  private.
- Keep network access behind the injectable client boundary.
- Keep repository access behind validated `Repository` and injectable `FileSystem`
  boundaries. Hosts own prompts, permissions, database location, and telemetry setup.

## Integration

- Use `Harness` for durable sessions or one-shot turns; implement `Model` to supply a
  provider. Follow `crates/ag-harness/README.md` for construction and usage examples.
- Deny tools by default. Repository tools require a trusted Git executable outside the
  containing worktree; comparisons additionally require a host-selected, pinned
  `ComparisonBase`.
- Keep turn options explicit. Per-turn overrides must not silently replace session
  defaults, and configuration changes must invalidate incompatible continuations.

## Invariants

- Require an output schema for every request and validate every response locally.
- Return explicit errors for unsupported capabilities; never weaken the shared contract.
- Validate and retain provider metadata at construction.
- Keep response bodies and diagnostics bounded, and apply duration telemetry uniformly.
- Allow only one active turn per durable session. Report completion only after messages
  are committed; retain failed/interrupted turns without replaying them as completed
  history. Durable write records describe past operations, not current filesystem state.

## SQLite Invariants

- Prefer checked SQLx query macros. Use `sqlx::query_as!` with named row structs for
  row-mapped reads, and keep `crates/ag-harness/.sqlx/` metadata current for offline
  builds. Regenerate it with the live-database workflow in `CONTRIBUTING.md`.

## Documentation

Keep `crates/ag-harness/README.md` aligned with the public integration contract and
`docs/site/content/docs/architecture/ag-harness-design.md` aligned with runtime and
persistence boundaries. Preserve the public surface exercised by
`crates/ag-harness/tests/public_api.rs` when changing exports.
