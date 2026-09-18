# ag-runtime

Runtime composition, harness dispatch, and provider lifecycle.

## Boundaries

- Only `ag-worker` may depend on this crate. Only this crate may depend on `ag-agent`.
- Use `ag-contracts` for shared execution interfaces and data. Keep frontend policy,
  mailbox scheduling, and persistence out of this crate.
- Construct concrete adapters here and retain them behind runtime objects. Never return
  provider transports to application workflows.
- Preserve cancellation, cleanup, usage aggregation, and provider-call budgets when
  forwarding admitted work to harness adapters.
- Keep raw adapter fixtures behind `test-utils`; consumers use worker test facilities.

## Documentation

Keep `docs/site/content/docs/core-components/execution.md`,
`docs/site/content/docs/architecture/runtime-flow.md`, and
`docs/site/content/docs/architecture/testability-boundaries.md` aligned with runtime
ownership.
