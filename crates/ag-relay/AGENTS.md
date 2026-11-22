# ag-relay

Relay for remote control: it joins a device's WebSocket to one that an Agentty instance
dials out, so the instance needs no open port.

## Boundaries

- Never read, decrypt, or log stream content. Devices and instances encrypt end to end;
  the relay only copies binary messages. Logs and metrics stay content-free.
- Keep the relay stateless: registrations and streams live in memory, and a restart only
  makes clients reconnect. Do not add accounts or a database.
- Route only by instance identifier, the fingerprint of the key the instance proves with
  the `ag-noise` relay handshake. Stream identifiers are unguessable and reach only that
  instance's control connection.
- Keep every limit in `RelayLimits`. A new resource a client can hold needs a limit, and
  every send goes through `Peer` so a client that stops reading cannot hold a task.
- Plain TCP connections and slow request headers are not limited here; the relay is
  meant to run behind a reverse proxy that terminates TLS. Say so wherever deployment is
  documented.
- Keep the crate independent of Agentty application crates. It embeds the web client
  files from `crates/ag-remote/assets/` by path and only marks the page for the relay
  transport; change the client there.

## Integration

- Consult `crates/ag-relay/src/lib.rs` for the endpoint and control-message contract.
  Changing a path or control message breaks released Agentty versions; add a new
  versioned path instead.

## Documentation

Keep `docs/site/content/docs/usage/remote-control.md` aligned when relay behavior,
limits, or what the relay can observe changes. `container/relay.Containerfile` builds
the relay's image; the `relay-image` hook builds and probes it, and
`.github/workflows/publish-relay-image.yml` publishes it on pushes to `main` and for
each release. Keep that workflow's probe aligned with the hook.
