# ag-forge

Provider-neutral forge review-request orchestration and remote parsing.

## Boundaries

- Keep shared contracts and dispatch provider-neutral; isolate CLI arguments, payloads,
  and parsing in provider adapters.
- Route every subprocess through the existing command-runner boundary.
- Expose normalized forge types to callers, never provider wire formats.
- When provider support changes, keep documentation aligned for every supported forge
  family and CLI.
