# Runtime Module

Terminal runtime loop and mode dispatch.

## Entry Points

- `core.rs` owns the foreground terminal lifecycle and main event loop.
- `event.rs` owns input polling and app-event integration.
- `key_handler.rs` and `mode.rs` route key handling by `AppMode`.

## Change Guidance

- Keep runtime orchestration free of direct host filesystem access.
- Dispatch pasted-image intents to the app layer; keep clipboard access, persistence,
  path canonicalization, and external-path validation behind infra traits.
