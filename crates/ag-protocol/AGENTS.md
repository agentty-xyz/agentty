# AG-Protocol

Shared protocol crate for Agentty agent response payloads, prompt envelopes, and turn
prompt transport models.

## Entry Points

- `src/lib.rs` is the public crate root.
- `src/model.rs` owns structured assistant response payloads and request profiles.
- `src/envelope.rs` owns structured-response prompt envelopes, compact refresh
  reminders, and protocol repair prompts backed by checked-in markdown templates under
  `src/template/`.
- `src/parse.rs` owns strict protocol parsing, recovery, normalization, and debug
  diagnostics.
- `src/schema.rs` owns JSON Schema generation and provider transport normalization.
- `src/prompt.rs` owns transport-neutral turn prompt payloads and attachment ordering.
- `src/question.rs` owns clarification question payloads used by the response schema.
- `src/subtask.rs` owns orchestrator subtask payloads used by the response schema.

## Change Guidance

- Keep this crate independent from `agentty` UI, runtime, persistence, and provider
  process orchestration.
- Add protocol model, schema, parser, prompt-envelope, and prompt-payload behavior here
  when it must be shared by multiple frontends or agent transports.
