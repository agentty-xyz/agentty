# ag-clipboard

Read-only clipboard support for Agentty prompt image capture.

## Boundaries

- Keep the public API synchronous; Agentty performs clipboard reads on a blocking
  thread.
- Keep platform details under `crates/ag-clipboard/src/backend/`.
- Preserve the audited Wayland path through the `wl-paste` subprocess backend rather
  than adding Rust Wayland protocol crates.
- Add clipboard writes only with a specified user-facing copy feature.

## Integration

- Hosts own asynchronous offloading, temporary image files, and attachment metadata;
  this crate returns clipboard data and typed `ClipboardError` failures.
- Handle unavailable backends as a supported runtime outcome. Consult
  `crates/ag-clipboard/src/lib.rs` for the read contract.

## Documentation

Update `docs/site/content/docs/usage/workflow.md` for paste behavior and platform
availability changes.
