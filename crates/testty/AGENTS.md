# testty

Published Rust-native TUI E2E framework and language-agnostic `testty` CLI.

## Public API

- Keep public items module-qualified; do not add crate-root re-exports. Keep renderer
  plumbing private.
- Treat `tests/public_api.rs` as the compatibility tripwire. Update it deliberately with
  public-surface changes and document breaking changes in `README.md` and
  `docs/upgrading.md`; an intentional break requires a workspace major version.
- Preserve the layered assertion API: `match_*` returns structured `MatchResult` for
  composition, while `assert_*` and `recipe::expect_*` remain panic adapters.
- Preserve existing `#[non_exhaustive]` guarantees; compatibility tests must destructure
  those types with rest patterns and fallback arms.

## Boundaries

- Keep CLI verbs thin: parse and validate in `src/main.rs`, then delegate to the
  library. Update `README.md` and framework docs when CLI behavior changes.
- In tests, inject snapshot update mode through `SnapshotConfig::with_update_mode()`; do
  not mutate process-global environment variables.
- Keep proof-backend geometry and rendering plumbing internal unless it is intentionally
  added to the curated public API.

`testty` shares the workspace version and release. Do not version or publish it
independently; keep `.github/workflows/publish-crates-io.yml` ordered correctly.
