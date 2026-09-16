# ag-agent

External-agent discovery and transport adapters implementing `ag-runtime` contracts.

## Boundaries

- Keep provider routing, prompt translation, CLI/app-server transports, retries, and
  resource cleanup here. Shared execution contracts belong in `ag-runtime`, selection
  models in `ag-session`, and scheduling in `ag-worker`.
- Keep concrete transports and parsers private; hosts use the curated crate-root API.
- Keep subprocess execution behind the shared internal executor and injected transport
  boundaries.

## Integration

- Compose adapters through the public factories at the host composition boundary.
  Agentty passes `AgentChannel` and `OneShotClient` to the worker; its application
  workflows submit through `ag-worker::RunClient` and must not execute raw runtime
  clients. Callers must not select transport-specific helpers.
- Preserve cancellation cleanup and charge every provider attempt, including retries and
  protocol repairs, against the supplied `ProviderCallBudget`.
- Await session cleanup after provider-turn panics. Adapters that detach cleanup work
  must implement forced shutdown so the host deadline can release their owned runtimes.
- Use the `test-utils` mocks and factories for deterministic host tests.

## Documentation

Follow `docs/site/content/docs/architecture/change-recipes.md` for provider and utility
prompt changes. Keep `docs/site/content/docs/architecture/runtime-flow.md` aligned with
transport lifecycle changes and prompt templates synchronized with their renderers.
