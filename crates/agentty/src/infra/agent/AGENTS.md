# Agent Infrastructure Module

Provider-specific command builders and response parsing helpers for agent CLI integrations.

## Docs Sync

When backend protocol or provider parsing behavior changes, update:

- `docs/site/content/docs/agents/backends.md` — backend capabilities, structured response protocol, resume behavior, and provider settings.
- `docs/site/content/docs/architecture/runtime-flow.md` — protocol parsing/validation and turn interaction flow.
- `docs/site/content/docs/architecture/module-map.md` — module responsibility descriptions for `infra/agent/`.

## Directory Index

- [`app_server/`](app_server/) - Provider-specific app-server client implementations kept private to agent backends.
- [`app_server.rs`](app_server.rs) - Router-only module for provider-specific app-server clients under `agent/`.
- [`backend.rs`](backend.rs) - Shared backend trait and transport request types.
- [`cli/`](cli/) - Shared CLI subprocess stdin/error helpers reused by turns and one-shot prompts.
- [`cli.rs`](cli.rs) - Router-only shared CLI helper module.
- [`claude.rs`](claude.rs) - Claude CLI backend command construction.
- [`codex.rs`](codex.rs) - Codex backend runtime command construction.
- [`gemini.rs`](gemini.rs) - Gemini backend runtime command construction.
- [`prompt.rs`](prompt.rs) - Shared prompt preparation helpers for transcript replay and protocol instruction injection.
- [`provider.rs`](provider.rs) - Central provider registry for transport policy, parsers, and app-server client factories.
- [`protocol/`](protocol/) - Structured response protocol subsystem split into model, schema, and parse modules.
- [`protocol.rs`](protocol.rs) - Protocol module router and public re-exports for structured response handling.
- [`response_parser.rs`](response_parser.rs) - Provider-specific parsing for final and streaming output.
- [`submission.rs`](submission.rs) - Shared one-shot prompt execution with strict protocol parsing and direct schema-error surfacing.
- [`template/`](template/) - Shared Askama template files used by agent-backed prompts.
- [`AGENTS.md`](AGENTS.md) - Local module guidance and directory index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
