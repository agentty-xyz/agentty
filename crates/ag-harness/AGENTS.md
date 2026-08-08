# `ag-harness`

Provider-neutral LLM harness for application-facing model calls. Keep the crate small,
typed, and independent of Agentty UI or orchestration concerns.

## Boundaries

- `Model` is the object-safe application boundary; `ModelClient` implements it and owns
  the shared request lifecycle, telemetry, and structured-output validation.
- Private provider modules own public configuration and provider-specific capability
  policy.
- API-family modules own shared authentication, payload translation, wire handling, and
  raw generation, but their runtime types remain private.
- Network access stays behind an injectable client boundary.

## Invariants

- Every request requires an output schema and every response is validated locally.
- Unsupported provider capabilities return explicit errors; backends must not silently
  weaken the shared contract.
- Provider metadata is validated and retained when a `ModelClient` is constructed.
- Response bodies and diagnostics remain bounded.
- Request-duration telemetry applies uniformly to every `ModelClient` request.

## Change Routing

- Put neutral lifecycle and contract changes in the model and schema modules.
- Put reusable API-protocol behavior in its API-family module.
- Keep provider-specific configuration and policy under the provider module.
- Add focused tests for lifecycle, transport, schema, and provider behavior.

## Documentation

Update `docs/site/content/docs/architecture/ag-harness-design.md` when these boundaries
or the planned runtime direction change.
