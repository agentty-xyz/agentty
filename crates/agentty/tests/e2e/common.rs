//! Shared helpers and agentty-specific `Journey` builders for E2E tests.
//!
//! Extracts reusable utilities from the flat test file so each test module
//! can compose scenarios without duplicating setup boilerplate.

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin;
use testty::frame::TerminalFrame;
use testty::journey::Journey;
use testty::proof::gif::GifBackend;
use testty::proof::report::{ProofCapture, ProofReport};
use testty::region::Region;
use testty::session::PtySessionBuilder;
use testty::step::Step;

/// Save a proof report as an animated GIF when `TESTTY_PROOF_OUTPUT` is set.
///
/// When the environment variable `TESTTY_PROOF_OUTPUT` points to a directory,
/// the GIF is written there. Otherwise the call is a no-op, keeping the
/// workspace clean during normal test runs.
pub(crate) fn save_proof_gif(report: &ProofReport, name: &str) {
    let Some(dir) = proof_output_dir() else {
        return;
    };

    let path = dir.join(format!("{name}.gif"));
    report
        .save(&GifBackend::new(), &path)
        .expect("failed to save proof GIF");

    println!("Proof GIF saved to {}", path.display());
}

/// Return the proof output directory from `TESTTY_PROOF_OUTPUT`, creating it
/// if needed. Returns `None` when the variable is unset.
fn proof_output_dir() -> Option<PathBuf> {
    let dir = std::env::var("TESTTY_PROOF_OUTPUT").ok()?;
    let path = Path::new(&dir).to_path_buf();
    std::fs::create_dir_all(&path).expect("failed to create proof output dir");

    Some(path)
}

/// Reconstruct a [`TerminalFrame`] from a [`ProofCapture`] so full cell-level
/// assertions (highlight, color, style) can be run against intermediate
/// captures.
pub(crate) fn frame_from_capture(capture: &ProofCapture) -> TerminalFrame {
    TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes)
}

/// Header region covering the title bar and tab bar (rows 0-3).
///
/// Agentty renders a title bar at row 0 and a tab bar at row 2. This region
/// is wider than a single row so tab-related assertions match the actual
/// layout.
pub(crate) fn header_region(cols: u16) -> Region {
    Region::new(0, 0, cols, 4)
}

/// Create a [`PtySessionBuilder`] with a clean isolated environment.
///
/// Sets `AGENTTY_ROOT` to a temporary directory, creates a deterministic
/// `test-project` working directory so frame snapshots are stable across
/// machines, and uses 80x24 terminal.
///
/// # Errors
///
/// Returns an error if directory creation fails.
pub(crate) fn test_builder(temp_root: &Path) -> std::io::Result<PtySessionBuilder> {
    let agentty_root = temp_root.join("agentty_root");
    let workdir = temp_root.join("test-project");

    std::fs::create_dir_all(&agentty_root)?;
    std::fs::create_dir_all(&workdir)?;

    Ok(PtySessionBuilder::new(cargo_bin("agentty"))
        .size(80, 24)
        .env("AGENTTY_ROOT", agentty_root.to_string_lossy())
        .workdir(workdir))
}

// ---------------------------------------------------------------------------
// Agentty-specific Journey builders
// ---------------------------------------------------------------------------

/// Wait for agentty to start up and render a stable initial frame.
///
/// Uses the standard startup timeouts: 500ms stability window with a
/// 10-second maximum wait.
pub(crate) fn wait_for_agentty_startup() -> Journey {
    Journey::new("agentty_startup")
        .with_description("Wait for agentty startup and initial render")
        .step(Step::wait_for_stable_frame(500, 10000))
}

/// Switch to a tab by pressing `Tab` and waiting for the tab label text.
///
/// Useful for navigating forward through the tab bar one step at a time.
pub(crate) fn switch_to_tab(tab_name: &str) -> Journey {
    Journey::new(format!("switch_to_{tab_name}"))
        .with_description(format!("Press Tab and wait for '{tab_name}' to appear"))
        .step(Step::press_key("Tab"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Switch to a tab by pressing `BackTab` and waiting for stability.
///
/// Useful for navigating backward through the tab bar one step at a time.
pub(crate) fn switch_to_tab_reverse(tab_name: &str) -> Journey {
    Journey::new(format!("switch_back_to_{tab_name}"))
        .with_description(format!("Press BackTab and wait for '{tab_name}' to appear"))
        .step(Step::press_key("BackTab"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Open the quit confirmation dialog by pressing `q`.
///
/// Waits for the dialog to render with a stable frame.
pub(crate) fn open_quit_dialog() -> Journey {
    Journey::new("open_quit_dialog")
        .with_description("Press q and wait for quit confirmation dialog")
        .step(Step::press_key("q"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Open the help overlay by pressing `?`.
///
/// Waits for the overlay to render with a stable frame.
pub(crate) fn open_help_overlay() -> Journey {
    Journey::new("open_help_overlay")
        .with_description("Press ? and wait for help overlay")
        .step(Step::press_key("?"))
        .step(Step::wait_for_stable_frame(300, 3000))
}
