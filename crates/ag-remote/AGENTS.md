# ag-remote

Remote control server: it registers with a relay and serves the devices that connect
through it, driving sessions through `ag_session::SessionService`. It opens no listener
and serves no web client; a program that embeds `ag-relay` serves the client.

## Boundaries

- Keep Agentty UI state, SQLite rows, and the relay a host links to in the host. This
  crate receives a `SessionService`, a revision counter, and a `DeviceStore`.
- Expose only the operations in `RemoteRequest`. The user approved PNG and JPEG images
  in chat messages, which the host stores itself, stopping a turn, creating sessions,
  merging, publishing, syncing a session or a project, session settings, generating a
  graph proposal with `/graph`, answering the questions of a session's graph work, and
  the graph inspector's actions: accepting an output, retrying or replying to a node,
  resuming or cancelling a graph, and adding provider calls to its spent budget. A
  request about a project is for the host's active project unless it names another in
  `project_id`; where the host offers `projects`, it may name any project the host
  lists. Any other mutation, any operation that runs a host command, and any read
  outside a listed session or a listed project's status is a new security decision; do
  not extend the protocol without one.
- Every request added after the first protocol version names its capability; the three
  original mutations, `send_message`, `answer_questions`, and `cancel_session`, are
  always served. A request declares the operations it needs and how it queues in
  `RequestPolicy::of` in `crates/ag-remote/src/request.rs`, and one that names a project
  also needs `Projects`. It takes the project it names as a `RequestedProjectId` and the
  session it targets as a `RequestedSessionId`, each of which reaches the host only
  through its `listed`, the check that the host lists it. Every mutation is listed under
  Security in `docs/site/content/docs/usage/remote-control.md` and in `SECURITY.md`.
- The host enforces the same listed-project and listed-session scope and approved
  operations for remote requests, so a new operation also needs the host to serve it to
  remote control: mutations in `crates/agentty/src/app/command_gateway/command.rs`,
  reads in `crates/agentty/src/app/session_runtime.rs`. Keep this crate's own checks.
- Treat the relay as untrusted: devices talk only through `EncryptedTransport`,
  authenticate by the key their handshake proved, become paired only after host approval
  of a redeemed one-time code, and stay capped while unauthenticated. Persist only the
  domain-separated digest of a device key; changing how it is computed unpairs every
  device. Never log pairing codes or message content.

## Integration

- Hosts call `start` with the relay to register with, issue pairing codes on request,
  approve or deny the pairing requests listed in `RemoteServerStatus`, revoke devices
  through `RemoteServerHandle`, and `shutdown` when remote control stops. Consult
  `crates/ag-remote/src/lib.rs` for the contract.
- The `test-utils` feature exports `test_support`, an in-memory `DeviceStore` and a
  seeded `SessionBackend` that programs embedding the crate run a real server over in
  their own tests, as `crates/ag-remote/tests/server/` does through an in-process relay.
  Embedders assert on its seeded data, so changing that data changes the public API.
  Keep the module to those host ports; relay and device helpers stay in the tests.
- Web clients already deployed by platforms talk to this server, and to older releases,
  through a relay. Keep every frame byte-compatible: give new frame fields a default,
  omit them when empty, and offer each new request only through a `capabilities` entry.
  A new `ClientFrame` type is not additive: an older server closes the connection on it.
  Keep rejection reasons as they are, since clients match them. Bump `PROTOCOL_VERSION`
  only when a frame changes meaning or loses a field.
- Update `crates/ag-remote/tests/server/` together with
  `crates/ag-remote/src/protocol.rs`, and change the device handshake only together with
  `ag-noise`.

## Documentation

Keep `docs/site/content/docs/usage/remote-control.md` aligned with pairing, available
operations, and security behavior.
