# ag-protocol

Shared structured response, prompt-envelope, turn-payload, clarification, orchestration,
and verification contracts.

## Boundaries

- Keep this crate independent of Agentty UI, runtime, persistence, and provider process
  orchestration.
- Put behavior here only when models, schemas, parsing, envelopes, or turn payloads must
  be shared by multiple frontends or transports.
- Keep checked-in templates under `crates/ag-protocol/src/template/` synchronized with
  the envelope code that renders them.

## Integration

- Use the same `ProtocolRequestProfile` for schema selection and response parsing.
  Provider adapters choose `SchemaRequiredPolicy`; shared callers consume normalized
  protocol models.
- Reuse prompt-envelope and turn-payload helpers instead of constructing competing wire
  formats. Consult `crates/ag-protocol/src/lib.rs` for the public contracts and their
  owning modules.

## Documentation

Keep models, schema generation, parser behavior, and prompt instructions synchronized
when the wire contract changes. Update
`docs/site/content/docs/architecture/runtime-flow.md` for protocol delivery changes.
