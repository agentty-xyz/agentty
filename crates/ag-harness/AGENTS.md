# `ag-harness`

Provider-neutral LLM harness for application-facing model calls. Keep the crate small,
typed, and independent of Agentty UI or orchestration concerns.

## Boundaries

- `Model` owns the shared request lifecycle, telemetry, and structured-output
  validation.
- Provider adapters implement `ModelBackend` and own authentication, capability checks,
  payload translation, and raw generation.
- API-family modules may share wire handling across providers, but their types remain
  private.
- Network access stays behind an injectable client boundary.

## Invariants

- Every request requires an output schema and every response is validated locally.
- Unsupported provider capabilities return explicit errors; adapters must not silently
  weaken the shared contract.
- Response bodies and diagnostics remain bounded.
- Request-duration telemetry applies uniformly to every `ModelBackend`.

## Change Routing

- Put neutral lifecycle and contract changes in the model and schema modules.
- Put reusable API-protocol behavior in its API-family module.
- Keep provider-specific behavior under the provider module and in each adapter.
- Add focused tests for lifecycle, transport, schema, and provider behavior.

## Documentation

Update `docs/site/content/docs/architecture/ag-harness-design.md` when these boundaries
or the planned runtime direction change.
