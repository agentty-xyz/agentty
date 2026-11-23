# ag-relay

Relay for remote control: it joins a device's WebSocket to one that an Agentty instance
dials out, so the instance needs no open port.

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
  meant to run behind a reverse proxy that terminates TLS. Say so wherever deployment is
  documented.
- Keep the crate independent of Agentty application crates. It embeds the web client
  files from `crates/ag-remote/assets/` by path and only renders the page: it marks it
  for the relay transport and fills in the options another web application passes to
  serve it, escaping their text and refusing links to other origins. Change the client
  there. Page options never grant a device anything, and the content security policy
  names every origin the page connects to.

## Integration

- Consult `crates/ag-relay/src/lib.rs` for the endpoint and control-message contract.
  Changing a path or control message breaks released Agentty versions; add a new
  versioned path instead.

## Documentation

Keep `docs/site/content/docs/usage/remote-control.md` aligned when relay behavior,
limits, or what the relay can observe changes. `container/relay.Containerfile` builds
the relay's image, and `container/relay-probe.sh` probes it for the `relay-image` hook
and for `.github/workflows/publish-relay-image.yml`, which publishes it on pushes to
`main` and for each release. `container/relay.fly.toml` deploys the image on Fly.io;
keep its port, flags, and health check aligned with the image.
