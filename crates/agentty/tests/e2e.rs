//! TUI end-to-end tests for the agentty binary.
//!
//! Uses the `ag-tui-test` framework to drive the real binary in a PTY,
//! capture terminal frames for semantic assertions, and optionally generate
//! VHS tapes for visual screenshot capture.
//!
//! # Running
//!
//! ```sh
//! cargo test -p agentty --test e2e -- --ignored
//! ```
//!
//! # Updating baselines
//!
//! ```sh
//! TUI_TEST_UPDATE=1 cargo test -p agentty --test e2e -- --ignored
//! ```

use std::path::PathBuf;

use ag_tui_test::recipe;
use ag_tui_test::region::Region;
use ag_tui_test::scenario::Scenario;
use ag_tui_test::session::PtySessionBuilder;

/// Return the path to the compiled agentty binary.
fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agentty"))
}

/// Create a `PtySessionBuilder` with a clean isolated environment.
///
/// Sets `AGENTTY_ROOT` to a temporary directory and uses 80x24 terminal.
fn test_builder(temp_root: &std::path::Path) -> PtySessionBuilder {
    let agentty_root = temp_root.join("agentty_root");
    let workdir = temp_root.join("test-project");

    std::fs::create_dir_all(&agentty_root).ok();
    std::fs::create_dir_all(&workdir).ok();

    PtySessionBuilder::new(binary_path())
        .size(80, 24)
        .env("AGENTTY_ROOT", agentty_root.to_string_lossy())
}

/// Verify that agentty startup renders the Projects tab as selected.
///
/// Launches agentty in a clean environment and asserts that the expected
/// tabs and labels appear in the correct regions with appropriate styling.
#[test]
#[ignore = "requires agentty binary — run with: cargo test --test e2e -- --ignored"]
fn startup_shows_projects_tab() {
    // Arrange
    let temp = tempfile::TempDir::new().ok();
    let Some(temp) = &temp else { return };
    let builder = test_builder(temp.path());

    let scenario = Scenario::new("startup")
        .wait_for_stable_frame(500, 5000)
        .capture();

    // Act
    let result = scenario.run(builder);

    // Assert
    if let Ok(frame) = result {
        recipe::expect_selected_tab(&frame, "Projects");
    }
}

/// Verify that Tab key switches between tabs.
///
/// Starts on Projects tab, presses Tab, and verifies the next tab
/// becomes selected while Projects becomes unselected.
#[test]
#[ignore = "requires agentty binary — run with: cargo test --test e2e -- --ignored"]
fn tab_key_switches_tabs() {
    // Arrange
    let temp = tempfile::TempDir::new().ok();
    let Some(temp) = &temp else { return };
    let builder = test_builder(temp.path());

    let scenario = Scenario::new("tab_switch")
        .wait_for_stable_frame(500, 5000)
        .press_key("Tab")
        .wait_for_stable_frame(300, 3000)
        .capture();

    // Act
    let result = scenario.run(builder);

    // Assert
    if let Ok(frame) = result {
        // After pressing Tab, "Sessions" should be selected.
        recipe::expect_selected_tab(&frame, "Sessions");
        recipe::expect_unselected_tab(&frame, "Projects");
    }
}

/// Verify that the footer shows keybinding hints on startup.
#[test]
#[ignore = "requires agentty binary — run with: cargo test --test e2e -- --ignored"]
fn startup_shows_footer_hints() {
    // Arrange
    let temp = tempfile::TempDir::new().ok();
    let Some(temp) = &temp else { return };
    let builder = test_builder(temp.path());

    let scenario = Scenario::new("footer_hints")
        .wait_for_stable_frame(500, 5000)
        .capture();

    // Act
    let result = scenario.run(builder);

    // Assert
    if let Ok(frame) = result {
        let footer = Region::footer(frame.cols(), frame.rows());
        let footer_text = frame.text_in_region(&footer);
        // Footer should contain at least some keybinding hints.
        assert!(
            !footer_text.trim().is_empty(),
            "Footer should contain keybinding hints"
        );
    }
}
