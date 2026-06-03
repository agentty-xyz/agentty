# Proof pipeline

Capture labeled terminal states during scenario execution, render through swappable
backends.

```rust
use std::path::Path;

use testty::proof::frame_text::FrameTextBackend;
use testty::proof::gif::GifBackend;
use testty::proof::html::HtmlBackend;
use testty::proof::junit::JunitBackend;
use testty::proof::strip::ScreenshotStripBackend;
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;

let scenario = Scenario::new("startup_proof")
    .wait_for_stable_frame(300, 5_000)
    .capture_labeled("launched", "App reached stable state")
    .press_key("Tab")
    .wait_for_stable_frame(200, 3_000)
    .capture_labeled("navigated", "Switched to second tab");

let builder = PtySessionBuilder::new("/path/to/binary").size(80, 24);
let (_frame, report) = scenario.run_with_proof(builder).expect("failed");

report.save(&FrameTextBackend,       Path::new("proof.txt")).unwrap();
report.save(&ScreenshotStripBackend, Path::new("proof.png")).unwrap();
report.save(&GifBackend::default(),  Path::new("proof.gif")).unwrap();
report.save(&HtmlBackend,            Path::new("proof.html")).unwrap();
report.save(&JunitBackend,           Path::new("proof.xml")).unwrap();
```

## Backends

- **`FrameTextBackend`** → `.txt` — CI logs, quick inspection
- **`ScreenshotStripBackend`** → `.png` — review comments, docs
- **`GifBackend`** → `.gif` — PR descriptions, demos
- **`HtmlBackend`** → `.html` — detailed review with diffs and assertions
- **`JunitBackend`** → `.xml` — JUnit-XML for non-Rust CI ingestion

`JunitBackend` maps the report to a `<testsuites>`/`<testsuite>` for the scenario and
one `<testcase>` per assertion, with a `<failure>` child for every failed assertion
carrying the structured failure message. A capture with no assertions becomes a skipped
`<testcase>` so a documented step is not counted as a passing check, and colliding
test-case names gain a stable ` #N` suffix.

Diffs between consecutive captures are computed automatically — see
[Frame diffing](frame-diffing.md).

## Gallery

After a run writes several artifacts into one directory, `proof::gallery::write_gallery`
scans that directory and writes an aggregated `index.html` that links or embeds each
`.txt`, `.png`, `.gif`, and `.html` artifact. Artifacts are sorted by file name, so
prefix each with a zero-padded step number (for example, `01_launched.txt`) to make the
gallery reflect run order. The result is a single entry point for browsing every proof a
run produced.

```rust
use std::path::Path;

use testty::proof::gallery;

gallery::write_gallery(Path::new("proof-output")).unwrap();
```
