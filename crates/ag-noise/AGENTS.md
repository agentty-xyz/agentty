# ag-noise

Noise handshakes and encrypted sessions for remote control, built on `snow`.

## Boundaries

- Keep the crate free of I/O, async runtimes, and Agentty types. Callers move the
  produced bytes.
- Keep the handshake patterns and prologues fixed per role. Changing either breaks every
  paired device and relay; treat it as a protocol version change.
- Keep the cipher suite to what browsers provide through WebCrypto: X25519, AES-GCM, and
  SHA-256, so a browser can run the device handshake in plain JavaScript.
- Never log or format private keys or plaintext. Only public keys and fingerprints may
  leave the crate as text.
- Keep reassembly bounded: a session refuses frames above the limit its caller sets.

## Integration

- Consult `crates/ag-noise/src/lib.rs` for the handshake order of each pattern and the
  session framing contract.

## Documentation

Keep `docs/site/content/docs/usage/remote-control.md` aligned when key handling or the
encryption guarantees change, and record changes to patterns, the cipher suite, or the
trust model in `docs/site/content/docs/architecture/remote-control-design.md`.
