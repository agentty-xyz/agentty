# Upgrading

testty's documented stable surface is reached through each owning module path (for
example, `testty::scenario::Scenario`, `testty::session::PtySessionBuilder`). The
`tests/public_api.rs` tripwire locks down those items plus the documented auxiliary
items (`testty::recipe`, `testty::snapshot::DEFAULT_UPDATE_ENV_VAR`,
`SnapshotConfig::with_update_env_var`, `SnapshotConfig::with_update_mode`,
`SnapshotConfig::is_update_mode`). Any breaking change to one of those items requires a
`testty` major version bump.

## From earlier 0.x releases

- `ProofBackend::render` now takes a single `testty::proof::backend::RenderContext`
  argument instead of `(&ProofReport, &Path)`. Callers that render through
  `ProofReport::save(&backend, path)` are unaffected. Custom backends read the report
  and destination from the context:

  ```rust
  // Before
  fn render(&self, report: &ProofReport, output: &Path) -> Result<(), ProofError> {
      let html = build(report)?;
      std::fs::write(output, html)?;
      Ok(())
  }

  // After
  fn render(&self, context: &RenderContext<'_>) -> Result<(), ProofError> {
      let html = build(context.report)?;
      std::fs::write(context.output, html)?;
      Ok(())
  }
  ```

  `RenderContext` is `#[non_exhaustive]` so future render inputs can be added without
  another breaking change; build one with `RenderContext::new(report, output)` and read
  its `report` / `output` fields.

- `ProofError` is `#[non_exhaustive]`. Match arms over it need a fallback `_` arm so new
  error variants stay non-breaking.

- `CellStyle::from_cell` is no longer public — it borrowed a `vt100::Cell`, an
  implementation-detail type that must not appear in testty's public API. Obtain a
  `CellStyle` through `TerminalFrame::cell_style(row, col)` instead.

- The `testty::prelude` wildcard re-export module has been removed. Replace
  `use testty::prelude::*;` with explicit per-module imports for the items each call
  site actually uses, for example:

  ```rust
  // Before
  use testty::prelude::*;

  // After
  use testty::scenario::Scenario;
  use testty::session::PtySessionBuilder;
  ```

- The `artifact`, `calibration`, and `overlay` modules are removed. Use the
  [proof pipeline](proof-pipeline.md) and [snapshot](snapshots.md) helpers instead.

- `testty::snapshot::is_update_mode()` is removed. Use
  `SnapshotConfig::is_update_mode(&self)` — it resolves the per-config env var or the
  override set via `SnapshotConfig::with_update_env_var` /
  `SnapshotConfig::with_update_mode`.

- Snapshot updates still default to `TUI_TEST_UPDATE=1`. Custom CI integrations can
  override via `SnapshotConfig::with_update_env_var`; the default is exposed as
  `testty::snapshot::DEFAULT_UPDATE_ENV_VAR`.

- `SnapshotConfig` is `#[non_exhaustive]`. Replace `SnapshotConfig { .. }` literals with
  `SnapshotConfig::new(..)` plus `with_*` builders.

- `SnapshotError` and its struct variants (`Mismatch`, `MissingBaseline`,
  `FrameMismatch`) are `#[non_exhaustive]`. Match arms need a fallback `_` arm and field
  destructuring needs `..`. `MissingBaseline` now carries `update_env_var`.

- `feature::GifStatus` is `#[non_exhaustive]` and gained `Fresh { gif_path, hash }` and
  `Stale { gif_path, current, committed }` for
  `FeatureDemo::gif_mode(GifMode::CheckOnly)`. `CheckOnly` is read-only — never invokes
  VHS, never mutates the filesystem. `feature::compute_frame_hash` and
  `feature::hash_sidecar_path` are public so external tooling can reproduce the on-disk
  sidecar hash.

- The feature-demo surface (`FeatureDemo`, `FeatureMeta`, `FeatureResult`, `GifMode`,
  `GifStatus`) is reached through `testty::feature::*`.
