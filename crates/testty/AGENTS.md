# TUI E2E Testing Framework

Rust-native TUI end-to-end testing framework using PTY-driven semantic assertions,
native frame rendering, and VHS-driven GIF capture. Published to crates.io in lockstep
with the rest of the workspace via `.github/workflows/publish-crates-io.yml`; consumers
expect a stable curated API surface.

## Entry Points

- `src/lib.rs` is the public crate root and lists the user-facing modules.
- `src/prelude.rs` is the curated `use testty::prelude::*;` re-export set.
- `src/session.rs` owns PTY execution and runtime driving.
- `src/scenario.rs`, `src/step.rs`, and `src/assertion.rs` own the user-facing test API.
- `src/journey.rs` provides composable journey building blocks for declarative test
  authoring.
- `src/proof.rs` and `src/proof/` own the proof pipeline: report collection, backend
  trait, and output renderers.
- `src/feature.rs` provides the `FeatureDemo` builder for scenario execution with
  hash-cached VHS GIF generation.
- `README.md` is the primary usage guide and should stay aligned with the public API.

## Visibility Discipline

- The `renderer` module is `pub(crate)`. It is implementation detail for the proof
  backends and is not part of the public API.
- Anything new that is purely plumbing for backends (cell→pixel geometry, glyph
  blitting, etc.) belongs inside `pub(crate) mod`s — do not re-add external `pub mod`
  exports for internal helpers.
- Whenever a new user-facing type is added, re-export it from `prelude.rs` so the
  canonical first import remains complete.
- `tests/public_api.rs` is the compile-time tripwire that locks down the `prelude` items
  plus the documented auxiliary stable items (`testty::recipe`,
  `testty::snapshot::DEFAULT_UPDATE_ENV_VAR`, `SnapshotConfig::with_update_env_var`,
  `SnapshotConfig::with_update_mode`, `SnapshotConfig::is_update_mode`). Update it
  deliberately whenever the published surface changes and bump the testty major version
  in lockstep with the upgrade note in the testty `README.md`.

## Layered Assertion API

- `assertion::match_*` functions return `Result<(), Box<AssertionFailure>>` (aliased as
  `MatchResult`) and carry the structured failure context (`Expected` variant, optional
  `Region`, matched spans, frame excerpt, pre-formatted message). The failure is boxed
  so the `Ok` path stays a single pointer-sized return. Use this layer for any new
  composable, retrying, soft-batched, or proof-report surface.
- `assertion::assert_*` functions are thin panic adapters that delegate to the matching
  `match_*`. Keep them so historical tests that expected panic-on-failure stay
  byte-compatible without a major version bump.
- `AssertionFailure` and `Expected` are both `#[non_exhaustive]`. New fields and
  variants stay non-breaking as long as `tests/public_api.rs` still uses `..`
  rest-patterns and a fallback `_` arm when destructuring them, so update those tripwire
  patterns whenever the structured surface evolves.

## Snapshot Update Mode

Update mode is triggered by an environment variable whose name defaults to
`TUI_TEST_UPDATE`, exposed as `testty::snapshot::DEFAULT_UPDATE_ENV_VAR`. Downstream
crates that need a different name can override it via the
`SnapshotConfig::with_update_env_var` builder.

For tests and other programmatic callers, `SnapshotConfig::with_update_mode(bool)`
injects the update-mode decision directly and bypasses the env var entirely. Always
prefer this injected boundary over mutating process-global env state from a test — the
Rust 2024 `std::env::set_var` and `std::env::remove_var` APIs are `unsafe` because
cargo's parallel test harness can run other threads that read or write the environment
concurrently, and unique variable names do not make those calls safe.

## Releasing testty

testty is released in lockstep with the rest of the workspace. The crate inherits its
version from `[workspace.package]` in the root `Cargo.toml` via
`version.workspace = true`, and `.github/workflows/publish-crates-io.yml` runs
`cargo publish -p testty` alongside `ag-forge` and `agentty` on every version tag push.
Do not bump `crates/testty/Cargo.toml` independently.
