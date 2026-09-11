//! Showcase: Frame diffing engine for detecting terminal state changes.
//!
//! Demonstrates computing cell-level diffs between terminal frames,
//! extracting changed regions, and generating human-readable summaries.
//! The diff engine powers automatic change detection in proof reports.
//!
//! Run with: `cargo run --example frame_diffing -p testty`

use std::io::{self, Write};

use testty::diff::{CellChange, FrameDiff};
use testty::frame::TerminalFrame;

fn main() -> io::Result<()> {
    run(&mut io::stdout().lock())
}

fn run(output: &mut impl Write) -> io::Result<()> {
    writeln!(output, "=== Testty Frame Diffing Showcase ===\n")?;

    // --- Example 1: Identical frames ---
    writeln!(output, "--- Example 1: Identical Frames ---")?;
    let frame_a = TerminalFrame::new(40, 5, b"Hello, World!\nStatus: OK");
    let frame_b = TerminalFrame::new(40, 5, b"Hello, World!\nStatus: OK");
    let diff = FrameDiff::compute(&frame_a, &frame_b);

    writeln!(output, "  Identical: {}", diff.is_identical())?;
    writeln!(output, "  Summary: {:?}", diff.summary())?;
    writeln!(output)?;

    // --- Example 2: Text content change ---
    writeln!(output, "--- Example 2: Text Content Change ---")?;
    let before = TerminalFrame::new(40, 5, b"Counter: 0\nStatus: idle");
    let after = TerminalFrame::new(40, 5, b"Counter: 42\nStatus: running");
    let diff = FrameDiff::compute(&before, &after);

    writeln!(output, "  Identical: {}", diff.is_identical())?;
    writeln!(output, "  Summary: {:?}", diff.summary())?;

    let regions = diff.changed_regions();
    writeln!(output, "  Changed regions: {}", regions.len())?;
    for region in &regions {
        writeln!(
            output,
            "    Row {}, cols {}..{}: {:?}",
            region.region.row,
            region.region.col,
            region.region.col + region.region.width,
            region.change_type,
        )?;
    }
    writeln!(output)?;

    // --- Example 3: Multi-line update simulating a dashboard refresh ---
    writeln!(output, "--- Example 3: Dashboard Refresh ---")?;
    let dashboard_before = TerminalFrame::new(
        50,
        6,
        b"Dashboard\n  CPU: 23%\n  Mem: 512 MB\n  Disk: 45%\n  Net: 1.2 Mbps\nLast update: 10:30",
    );
    let dashboard_after = TerminalFrame::new(
        50,
        6,
        b"Dashboard\n  CPU: 67%\n  Mem: 1.1 GB\n  Disk: 45%\n  Net: 3.4 Mbps\nLast update: 10:31",
    );
    let diff = FrameDiff::compute(&dashboard_before, &dashboard_after);

    writeln!(output, "  Summary: {:?}", diff.summary())?;

    let regions = diff.changed_regions();
    writeln!(output, "  Changed regions: {}", regions.len())?;
    for region in &regions {
        writeln!(
            output,
            "    Row {}, cols {}..{}: {:?}",
            region.region.row,
            region.region.col,
            region.region.col + region.region.width,
            region.change_type,
        )?;
    }
    writeln!(output)?;

    // --- Example 4: Per-cell inspection ---
    writeln!(output, "--- Example 4: Per-Cell Inspection ---")?;
    let line_before = TerminalFrame::new(10, 1, b"ABCDE");
    let line_after = TerminalFrame::new(10, 1, b"AbCdE");
    let diff = FrameDiff::compute(&line_before, &line_after);

    write!(output, "  Cell changes: ")?;
    for col in 0..5 {
        let change = diff.cell_change(0, col);
        let marker = match change {
            Some(CellChange::Unchanged) | None => '.',
            Some(CellChange::TextChanged) => 'T',
            Some(CellChange::StyleChanged) => 'S',
            Some(CellChange::BothChanged) => 'B',
        };
        write!(output, "{marker}")?;
    }
    writeln!(output, "  (. = unchanged, T = text changed)")?;

    writeln!(output, "\n=== Frame diffing showcase complete! ===")?;

    Ok(())
}

#[cfg(test)]
#[path = "frame_diffing_test.rs"]
mod tests;
