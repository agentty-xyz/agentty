# Crates Directory

Contains the workspace member crates.

## Workspace Crates

- `crates/ag-clipboard/` holds read-only clipboard backends shared by Agentty prompt
  image capture.
- `crates/ag-forge/` holds forge review-request types and provider integrations shared
  across the workspace.
- `crates/ag-git/` holds reusable git, worktree, sync, rebase, and squash-merge
  operations shared across the workspace.
- `crates/ag-protocol/` holds structured agent response contracts, protocol parsing,
  schema generation, and transport-neutral turn prompt payloads shared across frontends
  and agent adapters.
- `crates/ag-xtask/` holds workspace maintenance utilities, including migration checks
  and generated workspace-map output.
- `crates/agentty/` is the main TUI application crate with `app`, `domain`, `infra`,
  `runtime`, and `ui` layers.
- `crates/testty/` provides the Rust-native TUI end-to-end testing framework and ships
  the language-agnostic `testty` command-line binary (`src/main.rs`).
