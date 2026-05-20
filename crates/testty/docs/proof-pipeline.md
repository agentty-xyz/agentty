# Proof pipeline

Capture labeled terminal states during scenario execution, render through swappable
backends.

```rust
use std::path::Path;
use testty::prelude::*;
use testty::proof::frame_text::FrameTextBackend;
use testty::proof::strip::ScreenshotStripBackend;
use testty::proof::gif::GifBackend;
use testty::proof::html::HtmlBackend;

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
```

## Backends

- **`FrameTextBackend`** → `.txt` — CI logs, quick inspection
- **`ScreenshotStripBackend`** → `.png` — review comments, docs
- **`GifBackend`** → `.gif` — PR descriptions, demos
- **`HtmlBackend`** → `.html` — detailed review with diffs and assertions

Diffs between consecutive captures are computed automatically — see
[Frame diffing](frame-diffing.md).
