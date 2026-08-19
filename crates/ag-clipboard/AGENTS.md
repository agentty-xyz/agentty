# ag-clipboard

Read-only clipboard support for Agentty prompt image capture.

## Boundaries

- Keep the public API synchronous; Agentty performs clipboard reads on a blocking
  thread.
- Keep platform details under `src/backend/`.
- Preserve the audited Wayland path through the `wl-paste` subprocess backend rather
  than adding Rust Wayland protocol crates.
- Add clipboard writes only with a specified user-facing copy feature.
