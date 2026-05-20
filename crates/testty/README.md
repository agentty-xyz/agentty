# testty

End-to-end testing for terminal apps. Launch your app the way a real user would, then
check what shows up on screen — text, colors, and highlights.

## Why

- **Tests what users actually see.** Runs your real app, not a mock, and checks the
  rendered screen.
- **Readable checks.** Assert on visible text, colors, and highlighted items, with
  ready-made helpers for tabs, dialogs, and footers.
- **Shareable proof.** Save what each test saw as plain text, a screenshot, an animated
  GIF, or an HTML report.

## Install

```toml
[dev-dependencies]
testty = "0.9"
tempfile = "3"
```

## Write a test

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

- [Getting started](docs/getting-started.md) — install, write your first test, run it
- [Scenarios](docs/scenarios.md) — describe a sequence of actions and waits
- [Assertions](docs/assertions.md) — check text, colors, and highlights on screen
- [Snapshots](docs/snapshots.md) — compare a test against a saved baseline
- [Proof pipeline](docs/proof-pipeline.md) — save what a test saw as text, image, GIF,
  or HTML
- [Journeys](docs/journeys.md) — reusable building blocks for tests
- [Frame diffing](docs/frame-diffing.md) — see what changed between two screens
- [Examples](docs/examples.md) — runnable example programs
- [Upgrading](docs/upgrading.md) — version migration notes

Full API reference: [docs.rs/testty](https://docs.rs/testty).

## License

Apache-2.0. See [LICENSE](../../LICENSE).
