# ag-runtime

Transport-independent contracts for session turns and isolated agent execution.

## Boundaries

- Keep provider implementations, persistence, scheduling, and frontend state out of this
  crate. Adapters implement these contracts; hosts compose and schedule them.
- Keep request settings, continuation state, events, and errors transport-neutral.
  Provider-specific diagnostics must not require callers to depend on transport types.

## Integration

- `AgentChannel` and `OneShotClient` define runtime adapter contracts; implementation
  factories belong in `ag-agent`. Within Agentty, the worker invokes these adapters and
  application workflows submit through `ag-worker::RunClient`.
- Derive protocol profiles from `AgentRequestKind`. Preserve adapter ownership of
  cancellation, resource cleanup, usage aggregation, and per-attempt budget enforcement.
- Use `test-utils` mocks when testing consumers without a provider process.

## Documentation

Read the trait contracts in `crates/ag-runtime/src/contract.rs` and
`crates/ag-runtime/src/one_shot.rs` when implementing an adapter. Keep
`docs/site/content/docs/architecture/runtime-flow.md` aligned with execution-contract
changes and `docs/site/content/docs/architecture/testability-boundaries.md` aligned with
adapter responsibilities.
