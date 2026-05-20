# testty

Rust-native end-to-end testing for terminal apps. Drive a real binary in a PTY, capture
location-aware terminal state, and assert on text, style, color, and regions.

## Why

- **Real PTY, real frames.** No mocks. `vt100` parses the same bytes the user would see.
- **Semantic assertions.** Match text, colors, styles, and regions — or use recipe
  helpers for tabs, dialogs, footers.
- **Proof artifacts.** Render captured journeys to text, PNG strips, animated GIFs, or
  self-contained HTML.

## Install

```toml
[dev-dependencies]
testty = "0.9"
tempfile = "3"
```

## Quick start

```rust
use testty::prelude::*;

#[test]
fn tab_switches_view() {
    let temp = tempfile::TempDir::new().unwrap();
    let builder = PtySessionBuilder::new(env!("CARGO_BIN_EXE_myapp"))
        .size(80, 24)
        .workdir(temp.path());

    let scenario = Scenario::new("tab_switch")
        .wait_for_stable_frame(500, 5_000)
        .press_key("Tab")
        .wait_for_stable_frame(300, 3_000)
        .capture();

    let frame = scenario.run(builder).expect("scenario failed");

    testty::recipe::expect_selected_tab(&frame, "Sessions");
    testty::recipe::expect_unselected_tab(&frame, "Projects");
}
```

```sh
cargo test -p my-app --test e2e
```

## Documentation

- [Getting started](docs/getting-started.md) — install, first test, run it
- [Scenarios](docs/scenarios.md) — steps, waits, PTY sessions, frames, regions
- [Assertions](docs/assertions.md) — `assert_*` / `match_*`, `SoftAssertions`, recipes
- [Snapshots](docs/snapshots.md) — frame-text and visual baselines
- [Proof pipeline](docs/proof-pipeline.md) — labeled captures, four output backends
- [Journeys](docs/journeys.md) — reusable scenario building blocks
- [Frame diffing](docs/frame-diffing.md) — cell-level diffs between frames
- [Examples](docs/examples.md) — runnable showcase examples
- [Upgrading](docs/upgrading.md) — version migration notes

Full API reference: [docs.rs/testty](https://docs.rs/testty).

## License

Apache-2.0. See [LICENSE](../../LICENSE).
