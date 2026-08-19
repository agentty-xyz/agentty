# ag-protocol

Shared structured response, prompt-envelope, turn-payload, clarification, orchestration,
and verification contracts.

## Boundaries

- Keep this crate independent of Agentty UI, runtime, persistence, and provider process
  orchestration.
- Put behavior here only when models, schemas, parsing, envelopes, or turn payloads must
  be shared by multiple frontends or transports.
- Keep checked-in templates under `src/template/` synchronized with the envelope code
  that renders them.
