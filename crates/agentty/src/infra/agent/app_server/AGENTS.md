# Agent App-Server Clients

## Overview

Provider-specific persistent app-server clients that stay hidden under
`infra/agent/` so runtime transport details live next to their matching
backend implementations.

## Directory Index

- [`codex.rs`](codex.rs) - Codex app-server transport and per-session runtime management.
- [`gemini.rs`](gemini.rs) - Gemini ACP transport and per-session runtime management.
- [`AGENTS.md`](AGENTS.md) - Local module guidance and directory index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
