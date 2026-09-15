# ag-forge

Provider-neutral forge review-request orchestration and remote parsing.

## Boundaries

- Keep shared contracts and dispatch provider-neutral; isolate CLI arguments, payloads,
  and parsing in provider adapters.
- Route every subprocess through the existing command-runner boundary.
- Expose normalized forge types to callers, never provider wire formats.

## Integration

- Inject `ReviewRequestClient` into host workflows; compose `RealReviewRequestClient` at
  the host boundary and use `test-utils` mocks for deterministic consumers.
- Use the remote detection and normalized inputs/results from this crate. Hosts own
  authentication setup and user-facing workflow policy; adapters own CLI translation.
- Consult `crates/ag-forge/src/client.rs` for the public operation contracts.

## Documentation

Keep `docs/site/content/docs/usage/forge-authentication.md` aligned with every supported
forge family and CLI. Update `docs/site/content/docs/usage/workflow.md` when publication
or review-comment behavior changes.
