# Frame diffing

`FrameDiff` computes cell-level differences between two `TerminalFrame`s and produces
human-readable summaries.

```rust
use testty::diff::FrameDiff;
use testty::frame::TerminalFrame;

let before = TerminalFrame::new(80, 24, b"Counter: 0");
let after  = TerminalFrame::new(80, 24, b"Counter: 42");

let diff = FrameDiff::compute(&before, &after);
assert!(!diff.is_identical());

for region in diff.changed_regions() {
    println!(
        "Row {}, cols {}..{}: {:?}",
        region.region.row,
        region.region.col,
        region.region.col + region.region.width,
        region.change_type,
    );
}

for line in diff.summary() {
    println!("{line}");
}
```

Diffs are computed automatically between consecutive captures in a `ProofReport` and
shown in the HTML backend.
