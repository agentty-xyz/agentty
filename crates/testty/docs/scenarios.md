# Scenarios

A `Scenario` is an ordered sequence of `Step`s describing a user journey. Build it
fluently, then run it in a PTY.

```rust
use testty::scenario::Scenario;

let scenario = Scenario::new("tab_switch")
    .wait_for_stable_frame(500, 5_000)
    .press_key("Tab")
    .wait_for_stable_frame(300, 3_000)
    .capture();
```

## Steps

- `.write_text("hi")` — type into the terminal
- `.press_key("Enter")` — send a named key
- `.sleep_ms(200)` — fixed pause
- `.wait_for_text("Ready", 5_000)` — poll until substring appears
- `.wait_for_stable_frame(500, 5_000)` — wait for rendering to settle
- `.eventually(timeout, poll, predicate)` — poll a predicate until it passes
- `.capture()` — snapshot the terminal
- `.capture_labeled("init", "App launched")` — snapshot for proof reports

**Supported keys:** `Enter`, `Tab`, `Escape`/`Esc`, `Backspace`, `Up`, `Down`, `Left`,
`Right`, `Home`, `End`, `Delete`, `PageUp`, `PageDown`, `Space`, `Ctrl+<letter>`.
Unknown keys are sent as raw bytes.

## Waiting strategies

Prefer `wait_for_stable_frame` over `sleep_ms` — it adapts to actual render speed.

Use `eventually` when you need a richer predicate than a substring. It polls the live
frame and surfaces the last `AssertionFailure` on timeout:

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

## PtySession

`PtySession` spawns the binary in a real PTY via `portable-pty`.

```rust
use testty::session::PtySessionBuilder;

let builder = PtySessionBuilder::new("/path/to/binary")
    .size(120, 40)
    .args(["--help"])
    .env("DATABASE_URL", "sqlite::memory:")
    .workdir("/tmp/test-workspace");

// Run a scenario:
let frame = scenario.run(builder).expect("failed");

// Or drive manually:
let mut session = builder.spawn()?;
session.write_text("hello")?;
session.press_key("Enter")?;
let frame = session.wait_for_text("response", std::time::Duration::from_secs(5))?;
```

Default terminal size is 80×24.

## TerminalFrame

A `TerminalFrame` is a `vt100`-parsed snapshot of the terminal.

```rust
let all_text   = frame.all_text();
let row_text   = frame.row_text(0);
let region_txt = frame.text_in_region(&region);

let matches = frame.find_text("Title");
let matches = frame.find_text_in_region("Title", &region);
```

Wide-character continuation cells are skipped; blank columns stay as spaces so column
positions stay stable.

## Region

```rust
use testty::region::Region;

Region::top_row(80);          // First row
Region::footer(80, 24);       // Last row
Region::full(80, 24);         // Whole screen
Region::left_panel(80, 24);   // Left half
Region::right_panel(80, 24);  // Right half
Region::top_left(80, 24);     // Top-left quadrant
Region::top_right(80, 24);    // Top-right quadrant
Region::percent(0, 0, 100, 60, 80, 24); // Top 60%
Region::new(10, 5, 30, 3);    // col, row, width, height
```

## MatchedSpan

`find_text` returns `Vec<MatchedSpan>` carrying position, color, and style:

```rust
let span = &frame.find_text("Projects")[0];
span.rect;              // Region
span.foreground;        // Option<CellColor>
span.background;        // Option<CellColor>
span.style;             // CellStyle
span.is_bold();
span.is_highlighted();  // bold | inverse | has background
span.has_fg(&color);
span.has_bg(&color);
```
