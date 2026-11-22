# ag-remote

Remote control server: an embedded web client and one WebSocket that drive sessions
through `ag_session::SessionService`.

## Boundaries

- Keep Agentty UI state, SQLite rows, and listener policy (address, enablement) in the
  host. This crate receives a `SessionService`, a revision counter, and a `DeviceStore`.
- Expose only the operations in `RemoteRequest`. Adding merge, publishing, settings, or
  any other mutation is a security decision; do not extend the protocol without one.
- Keep device tokens secret: persist only their SHA-256 hash, and never log tokens,
  pairing codes, or message content.
- Keep the web client in `crates/ag-remote/assets/` free of build steps and external
  resources, and render untrusted text with `textContent` so the content security policy
  can stay strict.

## Integration

- Hosts call `start` with a loopback address, issue pairing codes on request, revoke
  devices through `RemoteServerHandle`, and `shutdown` when remote control is disabled.
  Consult `crates/ag-remote/src/lib.rs` for the contract.
- Bump `PROTOCOL_VERSION` for any incompatible frame change, and update the web client
  and `crates/ag-remote/tests/server/` together with `crates/ag-remote/src/protocol.rs`.

## Documentation

Keep `docs/site/content/docs/usage/remote-control.md` aligned with pairing, available
operations, and security behavior.
