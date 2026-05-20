# Snapshots

Two modes:

- **Frame text** — compares terminal text content. Pure Rust.
- **Visual** — compares rendered pixels via VHS. Requires VHS installed.

## Frame text

```rust
use testty::snapshot::{self, SnapshotConfig};

let config = SnapshotConfig::new(
    "tests/e2e_baselines",   // committed baselines
    "tests/e2e_artifacts",   // failure artifacts (gitignored)
);

snapshot::assert_frame_snapshot_matches(
    &config,
    "startup_projects_tab",
    &frame.all_text(),
).expect("frame snapshot should match");
```

## Visual (VHS)

Compile a scenario into a VHS tape and execute it:

```rust
let tape = scenario.to_vhs_tape(
    &binary_path,
    Path::new("/tmp/screenshot.png"),
    &[("MY_ENV", "value")],
);

tape.write_to(Path::new("/tmp/test.tape"))?;
tape.execute(Path::new("/tmp/test.tape"))?;
```

## Update mode

Set `TUI_TEST_UPDATE=1` to overwrite baselines:

```sh
TUI_TEST_UPDATE=1 cargo test -p my-app --test e2e
```

The variable name is configurable with `SnapshotConfig::with_update_env_var`; the
default is exposed as `testty::snapshot::DEFAULT_UPDATE_ENV_VAR`.

For programmatic callers, inject the decision directly (avoids `unsafe` env mutation):

```rust
let config = SnapshotConfig::new("tests/baselines", "tests/artifacts")
    .with_update_mode(true);
```

## Visual diff tuning

```rust
let config = SnapshotConfig::new("tests/baselines", "tests/artifacts")
    .with_thresholds(
        30.0,  // per-pixel color distance (Euclidean RGB)
        10.0,  // max percent of differing pixels
    );
```
