# testty

[![crates.io](https://img.shields.io/crates/v/testty.svg)](https://crates.io/crates/testty)
[![docs.rs](https://img.shields.io/docsrs/testty)](https://docs.rs/testty)
[![license](https://img.shields.io/crates/l/testty.svg)](../../LICENSE)

[Documentation](docs/README.md) | [API reference](https://docs.rs/testty)

testty is a framework for end-to-end testing of terminal apps. It launches your real app
and checks what shows up on screen — text, colors, and highlights — with a single Rust
API.

## Installation

```toml
[dev-dependencies]
testty = "0.9"
tempfile = "3"
```

## Command-line binary

testty also ships a `testty` command-line front end so projects in any language — not
just Rust crates — can drive TUI scenarios, generate schemas, and inspect proof
artifacts. Install it with:

```sh
cargo install testty
```

```text
testty run <scenario.yaml> [--bin <BIN>] [--proof <DIR>]
testty schema
testty proof open <html>
testty proof gallery <dir>
testty update
```

- `run` — execute a scenario file against a TUI binary.
- `schema` — print the JSON schema for scenario files.
- `proof open` — open a single proof report.
- `proof gallery` — build a gallery from a directory of proof reports.
- `update` — update stored scenario snapshots to match current output.

The command-line binary is currently a stub: the command tree is in place, but every
verb prints a "not yet implemented" notice and exits non-zero until the behavior is
wired up.

## Capabilities

### Reliable • No flaky tests

- **Auto-wait.** `wait_for_stable_frame` and `eventually` wait for the screen to settle
  before asserting, so you never hard-code sleeps.
- **Screen-first assertions.** Check visible text, colors, and highlighted items, with
  ready-made helpers for tabs, dialogs, and footers.
- **Full isolation.** Each test runs your real binary in its own workspace, so tests
  never bleed into one another.

### Proof you can share

- **Snapshots.** Compare a run against a saved baseline — screen text or pixels.
- **Reports.** Save what each test saw as plain text, a screenshot, an animated GIF, or
  a self-contained HTML report.

## Examples

#### Write a test

```rust
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;

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

#### Wait for something to appear

```rust
use std::time::Duration;

use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

let scenario = Scenario::new("counter")
    .write_text("+++")
    .eventually(
        Duration::from_secs(5),
        Duration::from_millis(50),
        |frame| assertion::match_text_in_region(frame, "Counter: 3", &Region::full(80, 24)),
    )
    .capture();
```

#### Compare against a saved baseline

```rust
use testty::snapshot::{self, SnapshotConfig};

let config = SnapshotConfig::new("tests/baselines", "tests/artifacts");
snapshot::assert_frame_snapshot_matches(&config, "startup", &frame.all_text())
    .expect("snapshot should match");
```

#### Save a shareable report

```rust
use std::path::Path;

use testty::proof::html::HtmlBackend;
use testty::proof::junit::JunitBackend;

let (_frame, report) = scenario.run_with_proof(builder).expect("failed");
report.save(&HtmlBackend, Path::new("proof.html")).unwrap();
report.save(&JunitBackend, Path::new("proof.xml")).unwrap();
```

## Resources

- [Getting started](docs/getting-started.md) — install, write your first test, run it
- [Scenarios](docs/scenarios.md) — describe a sequence of actions and waits
- [Assertions](docs/assertions.md) — check text, colors, and highlights on screen
- [Snapshots](docs/snapshots.md) — compare a test against a saved baseline
- [Proof pipeline](docs/proof-pipeline.md) — save what a test saw as text, image, GIF,
  HTML, or JUnit-XML
- [Journeys](docs/journeys.md) — reusable building blocks for tests
- [Frame diffing](docs/frame-diffing.md) — see what changed between two screens
- [Examples](docs/examples.md) — runnable example programs
- [Upgrading](docs/upgrading.md) — version migration notes
- [API reference](https://docs.rs/testty) — full generated docs

## License

Apache-2.0. See [LICENSE](../../LICENSE).
