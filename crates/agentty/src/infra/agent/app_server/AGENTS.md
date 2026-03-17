# Agent App-Server Clients

## Overview

Provider-specific persistent app-server clients that stay hidden under
`infra/agent/` so runtime transport details live next to their matching
backend implementations.

## Directory Index

- [`codex/`](codex/) - Codex app-server runtime implementation details.
- [`codex.rs`](codex.rs) - Router-only Codex app-server module.
- [`gemini/`](gemini/) - Gemini ACP runtime implementation details.
- [`gemini.rs`](gemini.rs) - Router-only Gemini app-server module.
- [`AGENTS.md`](AGENTS.md) - Local module guidance and directory index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
