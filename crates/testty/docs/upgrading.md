# Upgrading

The curated stable surface lives at `testty::prelude` and is locked down by the
`tests/public_api.rs` tripwire. Any breaking change to a prelude item or to the
documented auxiliary items (`testty::recipe`,
`testty::snapshot::DEFAULT_UPDATE_ENV_VAR`, `SnapshotConfig::with_update_env_var`,
`SnapshotConfig::with_update_mode`, `SnapshotConfig::is_update_mode`) requires a
`testty` major version bump.

## From earlier 0.x releases

- Replace `use testty::scenario::Scenario;` / `use testty::session::*;` with
  `use testty::prelude::*;`.
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
  `GifStatus`) is re-exported from `testty::prelude`.
