# ag-xtask

Deterministic Rust-based workspace maintenance tasks.

## Boundaries

- Put each task's logic in a focused module and register its CLI dispatch in
  `crates/ag-xtask/src/main.rs`.
- Keep commands suitable for local and CI use.
- For workflows with multiple filesystem or process calls, inject a mockable boundary
  and test the orchestration without live side effects.

## Integration

- Keep task failures observable through a failing process exit status. Do not treat
  unreadable inputs as successful empty results.
- Register repository checks in `.pre-commit-config.yaml` so local and CI callers use
  the same task. Cover invocation behavior in process-level integration tests.

## Documentation

Keep the relevant `skills/development/` recipes aligned with maintenance workflows and
`docs/site/content/docs/architecture/change-recipes.md` aligned with migration guidance.
