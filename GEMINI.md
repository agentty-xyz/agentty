# Agent Manager

Project to manage agents.

## Project Facts
- Project is a Rust workspace.
- The `crates/` directory contains all workspace members.
- `am-cli`: A binary crate providing the CLI interface using Ratatui.

## Rust Project Style Guide
- Dependency versions and project information (version, authors) are managed in the root `Cargo.toml`.
- All workspace crates must use `workspace = true` for shared package metadata and dependencies.
- Use `ratatui` for terminal UI development.

## Quality Gates
To ensure code quality, run the following commands:
- **Test:** `cargo test`
- **Lint:** `cargo clippy -- -D warnings`
- **Format:** `cargo fmt --all -- --check`
