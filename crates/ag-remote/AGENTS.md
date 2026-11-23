# ag-remote

Remote control server: an embedded web client and one WebSocket that drive sessions
through `ag_session::SessionService`.

## Boundaries

- Keep Agentty UI state, SQLite rows, and listener policy (address, enablement) in the
  host. This crate receives a `SessionService`, a revision counter, and a `DeviceStore`.
- Expose only the operations in `RemoteRequest`. The user approved stopping a turn,
  creating sessions, merging, publishing, syncing a session or the active project, and
  session settings. Any other mutation, any operation that runs a host command, and any
  read outside a listed session or the active project's status is a new security
  decision; do not extend the protocol without one.
- Every mutation names its capability, checks that the host lists the session it
  targets, if it targets one, and is listed under Security in
  `docs/site/content/docs/usage/remote-control.md` and in `SECURITY.md`.
- Keep device tokens secret: persist only their SHA-256 hash, and never log tokens,
  pairing codes, or message content.
- Keep the connection defences: a redeemed pairing code yields a token only after host
  approval, every route refuses a `Host` outside loopback and `allowed_hosts`, and
  unauthenticated sockets stay capped.
- Treat the relay as untrusted: relay devices talk only through `EncryptedTransport`,
  authenticate by the key their handshake proved, and share the same pairing approval
  and unauthenticated-socket cap as local devices. Keep key digests domain-separated
  from token digests.
- Keep the web client in `crates/ag-remote/assets/` free of build steps and external
  resources, and render untrusted text with `textContent` so the content security policy
  can stay strict. Refer to its files by relative path: `ag-relay` serves the same files
  and may sit under a path prefix.
- Keep the relay device key non-extractable wherever the browser can store it. The
  encrypted copy in `device_key.mjs` is only for browsers that cannot, and must keep
  upgrading itself.

## Integration

- Hosts call `start` with a loopback address, issue pairing codes on request, approve or
  deny the pairing requests listed in `RemoteServerStatus`, revoke devices through
  `RemoteServerHandle`, and `shutdown` when remote control is disabled. Consult
  `crates/ag-remote/src/lib.rs` for the contract.
- Change `crates/ag-remote/assets/noise.mjs` together with `ag-noise`. The tests in
  `crates/ag-remote/tests/server/web_client.rs` run it in Node.js against a real relay
  and server.
- Keep frame changes additive: a self-hosted relay serves its own copy of the web
  client, so the page and Agentty may differ in version. Give new frame fields a
  default, omit them when empty, and offer each new request only through a
  `capabilities` entry. A new `ClientFrame` type is not additive: an older server closes
  the connection on it. Bump `PROTOCOL_VERSION` only when a frame changes meaning or
  loses a field.
- Update the web client and `crates/ag-remote/tests/server/` together with
  `crates/ag-remote/src/protocol.rs`.
- Design values and the self-hosted monospace face live in `design/tokens.json` and
  shared foundations in `design/base.css`. `crates/ag-remote/assets/tokens.css`,
  `crates/ag-remote/assets/fonts.css`, and `crates/ag-remote/assets/base.css` are
  generated from them, and this crate and `ag-relay` serve all three, in that order,
  ahead of `app.css` as `/app.css`. The font is a `data:` URI, which the content
  security policy allows with `font-src data:` and nothing else. Keep only the client's
  own layout sizes, the `--tone-*` aliases, and its own rules in `app.css`, with type,
  spacing, widths, and control heights from the token scales; every rule in `base.css`
  also applies to the client. The generator also writes the `theme-color` value in
  `crates/ag-remote/assets/index.html`; scripts read colors from the custom properties
  at runtime.

## Documentation

Keep `docs/site/content/docs/usage/remote-control.md` aligned with pairing, available
operations, and security behavior.
