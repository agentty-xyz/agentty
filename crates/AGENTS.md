# Crates Directory

Contains the workspace member crates.

## Workspace Crates

- `crates/ag-agent/` holds agent provider models, prompt templates, CLI and app-server
  backend transports, provider-neutral channel contracts, injectable one-shot
  submission, and app-server routing shared by Agentty.
- `crates/ag-clipboard/` holds read-only clipboard backends shared by Agentty prompt
  image capture.
- `crates/ag-forge/` holds forge review-request types and provider integrations shared
  across the workspace.
- `crates/ag-git/` holds reusable git, worktree, sync, rebase, and squash-merge
  operations shared across the workspace.
- `crates/ag-harness/` holds the application-facing LLM harness contract and model
  adapters, beginning with Qwen.
- `crates/ag-protocol/` holds structured agent response contracts, protocol parsing,
  schema generation, and transport-neutral turn prompt payloads shared across frontends
  and agent adapters.
- `crates/ag-session/` holds shared session identity, lifecycle, orchestration, project,
  personality, review, setting, clarification, and transcript models plus the complete
  session aggregates and frontend-neutral programmatic lifecycle API.
- `crates/ag-store/` holds reusable persistence contracts, SQLite repository adapters,
  connection setup, offline query metadata, and embedded migrations.
- `crates/ag-tui-text/` holds shared Ratatui text rendering helpers for markdown,
  mermaid diagrams, and terminal-width wrapping/truncation.
- `crates/ag-xtask/` holds workspace maintenance utilities, including migration checks.
- `crates/agentty/` is the main TUI application crate with `app`, `domain`, `infra`,
  `runtime`, and `ui` layers.
- `crates/testty/` provides the Rust-native TUI end-to-end testing framework and ships
  the language-agnostic `testty` command-line binary (`src/main.rs`).

## Release Workflow Sync

When adding or removing a workspace crate that is a dependency of any crate already
published to crates.io, update `.github/workflows/publish-crates-io.yml` in the same
change so the publish plan includes newly required crates before their dependents or
removes obsolete crate publish steps.
