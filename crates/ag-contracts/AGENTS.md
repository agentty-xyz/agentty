# ag-contracts

Shared execution requests, events, settings, errors, and adapter interfaces.

## Boundaries

- Keep provider implementations, factories, scheduling, persistence, and frontend state
  out of this crate. Shared wire payloads belong in `ag-protocol`.
- Execution implementations may depend on these contracts; contracts must never depend
  on execution implementations or application/session orchestration.
- Keep cancellation and provider-call budget semantics explicit in the interfaces.
- Expose scripted mocks through `test-utils` for deterministic consumer tests.

## Documentation

Keep `docs/site/content/docs/core-components/execution.md` and
`docs/site/content/docs/architecture/testability-boundaries.md` aligned with contract
changes.
