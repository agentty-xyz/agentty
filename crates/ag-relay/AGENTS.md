# ag-relay

Relay for remote control, as a library a program embeds: it joins a device's WebSocket
to one that an Agentty instance dials out, so the instance needs no open port.

## Boundaries

- Never read, decrypt, or log stream content. Devices and instances encrypt end to end;
  the relay only copies binary messages. Logs and metrics stay content-free.
- Keep the relay stateless: registrations and streams live in memory, and a restart only
  makes clients reconnect. Do not add accounts or a database. Which instances may
  register is decided by the `AdmissionPolicy` the embedder supplies; the relay stores
  no decisions and fails closed when a policy errs or stalls.
- Route only by instance identifier, the fingerprint of the key the instance proves with
  the `ag-noise` relay handshake. Stream identifiers are unguessable and reach only that
  instance's control connection.
- Keep every limit in `RelayLimits`. A new resource a client can hold needs a limit, and
  every send goes through `Peer` so a client that stops reading cannot hold a task.
- Plain TCP connections and slow request headers are not limited here; the relay is
  meant to run behind a reverse proxy that terminates TLS. Say so in the crate
  documentation.
- Keep the crate independent of Agentty application crates, and serve no page: the
  embedder serves the web client devices run.

## Integration

- Consult `crates/ag-relay/src/lib.rs` for the endpoint and control-message contract.
  Changing a path or control message breaks released Agentty versions; add a new
  versioned path instead. Embedders depend on `start`, `start_with_admission`, and the
  configuration, limit, handle, error, and admission types; change their signatures only
  as a breaking change.

## Documentation

Keep `docs/site/content/docs/usage/remote-control.md` aligned when relay behavior,
limits, or what the relay can observe changes.
