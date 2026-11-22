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
- Keep every limit in `RelayLimits`: handshakes in progress, instances in total and per
  address, streams per instance, bytes per minute, and the handshake and answer
  timeouts. A new resource a client can hold needs a limit.
- Keep the crate independent of Agentty application crates.

## Integration

- Consult `crates/ag-relay/src/lib.rs` for the endpoint and control-message contract.
  Changing a path or control message breaks released Agentty versions; add a new
  versioned path instead.

## Documentation

Keep `docs/site/content/docs/usage/remote-control.md` aligned when relay behavior,
limits, or what the relay can observe changes.
