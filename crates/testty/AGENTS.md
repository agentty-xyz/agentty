# testty

Published Rust-native TUI E2E framework and language-agnostic `testty` CLI.

## Public API

- Keep public items module-qualified; do not add crate-root re-exports. Keep renderer
  plumbing private.
- Treat `crates/testty/tests/public_api.rs` as the compatibility tripwire. Update it
  deliberately with public-surface changes; an intentional break requires a workspace
  major version.
- Preserve the layered assertion API: `match_*` returns structured `MatchResult` for
  composition, while `assert_*` and `recipe::expect_*` remain panic adapters.
- Preserve existing `#[non_exhaustive]` guarantees; compatibility tests must destructure
  those types with rest patterns and fallback arms.

## Boundaries

- Keep CLI verbs thin: parse and validate in `crates/testty/src/main.rs`, then delegate
  to the library.
- In tests, inject snapshot update mode through `SnapshotConfig::with_update_mode()`; do
  not mutate process-global environment variables.
- Keep proof-backend geometry and rendering plumbing internal unless it is intentionally
  added to the curated public API.

## Integration

- Use PTY assertions for terminal semantics and VHS captures for visual artifacts.
  Follow `crates/testty/README.md` and `crates/testty/docs/README.md` for host examples.
- Follow `crates/agentty/tests/e2e/AGENTS.md` when integrating Agentty feature
  scenarios.

## Documentation

Document public API and CLI changes in `crates/testty/README.md` and framework docs;
record breaking changes in `crates/testty/docs/upgrading.md`.

`testty` shares the workspace version and release. Do not version or publish it
independently; keep `.github/workflows/publish-crates-io.yml` ordered correctly.
