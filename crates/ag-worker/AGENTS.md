# ag-worker

Headless serial scheduling, cancellation, heartbeat coordination, and restart recovery.

## Boundaries

- Keep frontend, provider, Git, and SQLite implementations out of the worker. Hosts own
  workflow policy and ordered effects; runtime adapters own provider resources.
- Keep operation persistence generic over its error type through `OperationRepository`;
  concrete storage adapters belong in `ag-store`.

## Integration

- Implement `WorkQueue` and `WorkerHost` using one monotonically increasing submission
  order for commands and messages. Execution includes ordered post-processing before the
  next item may run.
- Notify the worker after queue or pause-state changes. On mailbox closure, settle
  abandoned work and notify its callers before releasing resources.
- Inject `Clock` for heartbeat timing. Before recovery, ensure the previous worker has
  stopped; reconcile host state before marking unfinished operations failed.
- Close session utility admission durably before reclaiming canceled session tracking.
  Hosts finish canceled utilities before deleting resources; session IDs are never
  reused.

## Documentation

Use `crates/ag-worker/tests/headless.rs` as a host-integration example and consult the
contracts in `crates/ag-worker/src/scheduler.rs` and
`crates/ag-worker/src/lifecycle.rs`. Keep
`docs/site/content/docs/architecture/runtime-flow.md` aligned with scheduling changes.
