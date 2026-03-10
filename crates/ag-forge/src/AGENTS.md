# AG-Forge Source

Forge review-request types, adapter routing, and CLI-backed provider integrations.

## Directory Index

- [`AGENTS.md`](AGENTS.md) - Source directory guidance and index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
- [`client.rs`](client.rs) - Public review-request trait boundary and production adapter dispatch.
- [`command.rs`](command.rs) - Mockable forge CLI command runner and shared command output helpers.
- [`github.rs`](github.rs) - GitHub pull-request adapter built around `gh`.
- [`gitlab.rs`](gitlab.rs) - GitLab merge-request adapter built around `glab`.
- [`lib.rs`](lib.rs) - Crate root router and public re-exports.
- [`model.rs`](model.rs) - Shared forge review-request types and normalized errors.
- [`remote.rs`](remote.rs) - Remote parsing and supported-forge detection helpers.
