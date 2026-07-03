# ag-clipboard

Read-only clipboard support crate for Agentty prompt image capture.

## Purpose

- Exposes the narrow clipboard surface Agentty needs: text, file-list, and RGBA image
  reads.
- Owns platform clipboard details so `crates/agentty/` can keep prompt image persistence
  behind `ClipboardImageClient`.

## Boundaries

- Keep the public API synchronous; Agentty runs clipboard reads on a blocking thread.
- Keep platform integrations inside `src/backend/`.
- Preserve audit-clean Wayland behavior: Wayland reads go through the approved
  `wl-paste` subprocess backend instead of Rust Wayland protocol crates.
- Do not add write support until a user-facing copy feature is specified.
