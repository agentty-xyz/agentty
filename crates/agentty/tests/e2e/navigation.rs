//! Navigation E2E tests: tab cycling, reverse tab cycling, and help overlay.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Seeds a GitHub remote and `gh` stub that returns personal and group-sourced
/// requested reviews for the Review tab feature scenario.
fn seed_review_tab_requested_reviews(env: &BuilderEnv) -> E2eResult {
    let output = Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/agentty-xyz/agentty.git",
        ])
        .current_dir(&env.workdir)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git remote add origin failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let gh_stub = env.stub_bin.join("gh");
    std::fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  exit 0
fi

if [ "$1" = "search" ] && [ "$2" = "prs" ] && [ "$3" = "--review-requested" ]; then
  cat <<'JSON'
[
  {
    "isDraft": false,
    "number": 42,
    "title": "Review personal parser",
    "updatedAt": "2026-04-27T21:30:00Z",
    "url": "https://github.com/agentty-xyz/agentty/pull/42"
  },
  {
    "isDraft": false,
    "number": 43,
    "title": "Review team parser",
    "updatedAt": "2026-04-28T21:30:00Z",
    "url": "https://github.com/agentty-xyz/agentty/pull/43"
  }
]
JSON
  exit 0
fi

if [ "$1" = "search" ] && [ "$2" = "prs" ] && [ "$3" = "user-review-requested:@me" ]; then
  cat <<'JSON'
[
  {
    "isDraft": false,
    "number": 42,
    "title": "Review personal parser",
    "updatedAt": "2026-04-27T21:30:00Z",
    "url": "https://github.com/agentty-xyz/agentty/pull/42"
  }
]
JSON
  exit 0
fi

echo "unexpected gh args: $*" >&2
exit 1
"#,
    )?;

    #[cfg(unix)]
    std::fs::set_permissions(&gh_stub, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Verify that agentty startup renders the Projects tab as selected.
///
/// Launches agentty in a clean environment and asserts that the expected
/// tabs and labels appear in the correct regions with appropriate styling.
#[test]
fn startup_shows_projects_tab() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("startup")
        .with_git()
        .zola(
            "Startup",
            "Initial render with the Projects tab selected.",
            10,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(3000)
                    .capture_labeled("startup", "Initial render with Projects tab")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agentty", &full);
                assertion::assert_text_in_region(frame, "test-project", &full);
            },
        )?;

    Ok(())
}

/// Verify that Tab key switches between tabs.
///
/// Starts on Projects tab, presses Tab, and verifies the next tab
/// becomes selected while Projects becomes unselected.
#[test]
fn tab_key_switches_tabs() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("tab_switch")
        .with_git()
        .zola(
            "Tab switching",
            "Jump between workspace tabs with a single keypress.",
            60,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .capture_labeled("before", "Projects tab selected")
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2500)
                    .capture_labeled("after", "Sessions tab selected")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "No sessions", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing Tab cycles through all primary tabs in order.
///
/// Starts on Projects and asserts each successive tab becomes selected:
/// Sessions, Review, Settings.
#[test]
fn tab_cycles_through_all_tabs() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("tab_full_cycle")
        .with_git()
        .zola(
            "Full tab cycle",
            "Cycle through every workspace tab in order.",
            70,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("sessions", "Sessions tab selected")
                    .compose(&common::switch_to_tab("Review"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("review", "Review tab selected")
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2500)
                    .capture_labeled("settings", "Settings tab selected")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Reasoning Level", &full);

                assert_eq!(
                    report.captures.len(),
                    3,
                    "Expected 3 captures (sessions, review, settings)"
                );

                let sessions_frame = common::frame_from_capture(&report.captures[0]);
                let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
                assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

                let review_frame = common::frame_from_capture(&report.captures[1]);
                let review_full = Region::full(review_frame.cols(), review_frame.rows());
                assertion::assert_text_in_region(&review_frame, "Review Requests", &review_full);
            },
        )?;

    Ok(())
}

/// Verify that the Review tab renders requested PR/MR review state.
#[test]
fn review_tab_shows_requested_reviews_page() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_tab")
        .with_git()
        .setup(seed_review_tab_requested_reviews)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Review"))
                    .wait_for_text("Requested from you", 5000)
                    .wait_for_text("Requested from your groups", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("review", "Review tab selected")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Review Requests", &full);
                assertion::assert_text_in_region(frame, "Requested from you", &full);
                assertion::assert_text_in_region(frame, "PR #42 Review personal parser", &full);
                assertion::assert_text_in_region(frame, "Requested from your groups", &full);
                assertion::assert_text_in_region(frame, "PR #43 Review team parser", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `q` opens a quit confirmation dialog.
///
/// The dialog should display the title "Confirm Quit" and the message
/// "Quit agentty?" with selectable options.
#[test]
fn quit_shows_confirmation_dialog() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("quit_confirmation")
        .with_git()
        .zola(
            "Quit confirmation",
            "Confirm before quitting to prevent accidental exits.",
            130,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .capture_labeled("before", "App running before quit")
                    .compose(&common::open_quit_dialog())
                    .viewing_pause_ms(2500)
                    .capture_labeled("dialog", "Quit confirmation dialog")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Confirm Quit", &full);
                assertion::assert_text_in_region(frame, "Quit agentty?", &full);
            },
        )?;

    Ok(())
}

/// Verify that the footer shows keybinding hints on startup.
#[test]
fn startup_shows_footer_hints() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("footer_hints")
        .with_git()
        .zola(
            "Footer hints",
            "Context-sensitive hints in the footer guide available actions.",
            110,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(3000)
                    .capture_labeled("startup", "Footer with keybinding hints")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "q: quit", &full);
                assertion::assert_text_in_region(frame, "?: help", &full);
            },
        )?;

    Ok(())
}

/// Verify that `BackTab` (Shift+Tab) cycles tabs in reverse order.
///
/// Starts on Projects (first tab), navigates forward to Settings (last tab),
/// then presses `BackTab` to cycle back through Review, Sessions, and
/// Projects.
#[test]
fn backtab_cycles_tabs_reverse() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("backtab_reverse")
        .with_git()
        .zola(
            "Reverse tab navigation",
            "Navigate tabs in reverse with Shift+Tab.",
            80,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Review"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("at_settings", "Settings tab selected before reverse")
                    .compose(&common::switch_to_tab_reverse("Review"))
                    .viewing_pause_ms(1500)
                    .capture_labeled("back_to_review", "Review tab after first BackTab")
                    .compose(&common::switch_to_tab_reverse("Sessions"))
                    .viewing_pause_ms(1500)
                    .capture_labeled("back_to_sessions", "Sessions tab after second BackTab")
                    .compose(&common::switch_to_tab_reverse("Projects"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_projects", "Projects tab after third BackTab")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "test-project", &full);

                let review_frame = common::frame_from_capture(&report.captures[1]);
                let review_full = Region::full(review_frame.cols(), review_frame.rows());
                assertion::assert_text_in_region(&review_frame, "Review Requests", &review_full);

                let sessions_frame = common::frame_from_capture(&report.captures[2]);
                let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
                assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

                let projects_frame = common::frame_from_capture(&report.captures[3]);
                let projects_full = Region::full(projects_frame.cols(), projects_frame.rows());
                assertion::assert_text_in_region(&projects_frame, "test-project", &projects_full);
            },
        )?;

    Ok(())
}

/// Verify that `?` opens the help overlay with keybinding content, and
/// `Esc` closes it and restores the previous view.
#[test]
fn help_overlay_toggle() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("help_overlay")
        .with_git()
        .zola(
            "Help overlay",
            "Press ? to see available keybindings for the current view.",
            100,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .capture_labeled("before", "Normal view before help")
                    .compose(&common::open_help_overlay())
                    .viewing_pause_ms(2500)
                    .capture_labeled("help_open", "Help overlay visible")
                    .press_key("Escape")
                    .wait_for_stable_frame(300, 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("help_closed", "Help overlay dismissed")
            },
            |frame, report| {
                let help_frame = common::frame_from_capture(&report.captures[1]);
                let full = Region::full(help_frame.cols(), help_frame.rows());
                assertion::assert_text_in_region(&help_frame, "Keybindings", &full);

                let restored_full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "test-project", &restored_full);

                let closed_text = frame.text_in_region(&restored_full);
                assert!(
                    !closed_text.contains("Keybindings"),
                    "Help overlay should be dismissed after Esc"
                );
            },
        )?;

    Ok(())
}
