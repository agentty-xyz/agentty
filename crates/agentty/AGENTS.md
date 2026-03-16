# Agentty CLI

Modular TUI application for managing agent sessions.
The main binary is named `agentty`.

## Directory Index

- [`.sqlx/`](.sqlx/) - Checked-in SQLx offline query metadata for compile-time macro validation.
- [`migrations/`](migrations/) - SQLite schema migrations for SQLx.
- [`src/`](src/) - Source code directory.
- [`tests/`](tests/) - Integration tests for live provider behavior and protocol compliance.
- [`AGENTS.md`](AGENTS.md) - Crate documentation.
- [`askama.toml`](askama.toml) - Askama template discovery configuration for embedded prompt resources.
- [`Cargo.toml`](Cargo.toml) - Crate dependencies and metadata.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
