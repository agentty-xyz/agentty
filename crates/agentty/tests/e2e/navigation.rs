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
/// requested reviews for the Inbox tab feature scenario.
fn seed_inbox_tab_requested_reviews(env: &BuilderEnv) -> E2eResult {
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
        r###"#!/bin/sh
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
    "author": {"login": "octocat"},
    "body": "## Details\n- **Personal parser** review description.\n<details>\n<summary>Release notes</summary>\n<h2>v1.0.0</h2>\n<ul>\n<li>Fix <code>parser</code> output.</li>\n</ul>\n</details>",
    "updatedAt": "2026-04-27T21:30:00Z",
    "url": "https://github.com/agentty-xyz/agentty/pull/42"
  },
  {
    "isDraft": false,
    "number": 43,
    "title": "Review team parser",
    "author": {"login": "team-lead"},
    "body": "Team parser review description.",
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
    "author": {"login": "octocat"},
    "body": "## Details\n- **Personal parser** review description.\n<details>\n<summary>Release notes</summary>\n<h2>v1.0.0</h2>\n<ul>\n<li>Fix <code>parser</code> output.</li>\n</ul>\n</details>",
    "updatedAt": "2026-04-27T21:30:00Z",
    "url": "https://github.com/agentty-xyz/agentty/pull/42"
  }
]
JSON
  exit 0
fi

if [ "$1" = "api" ] && [ "$2" = "--hostname" ] && [ "$4" = "graphql" ]; then
  case "$*" in
    *"number=42"*)
      sleep 3
      cat <<'JSON'
{"data":{"repository":{"pullRequest":{"comments":{"nodes":[{"author":{"login":"alice"},"body":"General **review** comment."}]},"reviewThreads":{"nodes":[{"id":"thread-navigation","diffSide":"RIGHT","isOutdated":false,"isResolved":false,"line":7,"path":"crates/agentty/src/ui/page/review_detail.rs","startLine":null,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"bob"},"body":"Please show this selected review comment."}]}}]}}}}}
JSON
      exit 0
      ;;
    *"number=43"*)
      cat <<'JSON'
{"data":{"repository":{"pullRequest":{"comments":{"nodes":[]},"reviewThreads":{"nodes":[]}}}}}
JSON
      exit 0
      ;;
  esac
fi

echo "unexpected gh args: $*" >&2
exit 1
"###,
    )?;

    #[cfg(unix)]
    std::fs::set_permissions(&gh_stub, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Seeds a GitHub remote and `gh` stub that returns issues assigned in that
/// project.
fn seed_assigned_github_issues(env: &BuilderEnv) -> E2eResult {
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

if [ "$1" = "search" ] && [ "$2" = "issues" ]; then
  cat <<'JSON'
[
  {
    "number": 124,
    "title": "Keep issue list compact",
    "updatedAt": "2026-07-09T18:30:00Z",
    "url": "https://github.com/agentty-xyz/agentty/issues/124",
    "repository": {"nameWithOwner": "agentty-xyz/agentty"}
  }
]
JSON
  exit 0
fi

if [ "$1" = "issue" ] && [ "$2" = "view" ] && [ "$3" = "124" ]; then
  cat <<'JSON'
{
  "assignees": [{"login": "octocat"}],
  "author": {"login": "hubot"},
  "body": "Show the selected issue's base information without loading comments.",
  "createdAt": "2026-07-01T10:00:00Z",
  "labels": [{"name": "enhancement"}, {"name": "ui"}],
  "number": 124,
  "state": "OPEN",
  "title": "Keep issue list compact",
  "updatedAt": "2026-07-09T18:30:00Z",
  "url": "https://github.com/agentty-xyz/agentty/issues/124"
}
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

/// Seeds the canonical project as the persisted active project.
fn seed_active_project_setting(env: &BuilderEnv) -> E2eResult {
    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let canonical_workdir = env.workdir.canonicalize()?;
        let database = common::open_database(env).await?;
        let project_id = database
            .projects()
            .upsert_project(
                &canonical_workdir.to_string_lossy(),
                Some("main".to_string()),
            )
            .await?;
        agentty::test_support::persist_active_project_id_for_test(&database, project_id).await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Verify that agentty startup renders the Sessions tab when an active
/// project already exists.
///
/// Launches agentty in a clean environment and asserts that the expected
/// tabs and labels appear in the correct regions with appropriate styling.
#[test]
fn startup_shows_sessions_tab_for_active_project() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("startup")
        .with_git()
        .setup(seed_active_project_setting)
        .zola(
            "Startup",
            "Launch agentty and land on the session list in seconds.",
            10,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(3000)
                    .capture_labeled("startup", "Initial render with Sessions tab")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agentty", &full);
                assertion::assert_text_in_region(frame, "test-project", &full);
                assertion::assert_text_in_region(frame, "No sessions", &full);
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
/// Sessions, Inbox, Issues, Settings.
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
                    .compose(&common::switch_to_tab("Inbox"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("inbox", "Inbox tab selected")
                    .compose(&common::switch_to_tab("Issues"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("issues", "Issues tab selected")
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2500)
                    .capture_labeled("settings", "Settings tab selected")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Default Smart Model", &full);

                assert_eq!(
                    report.captures.len(),
                    4,
                    "Expected 4 captures (sessions, inbox, issues, settings)"
                );

                let sessions_frame = common::frame_from_capture(&report.captures[0]);
                let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
                assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

                let inbox_frame = common::frame_from_capture(&report.captures[1]);
                let inbox_full = Region::full(inbox_frame.cols(), inbox_frame.rows());
                assertion::assert_text_in_region(&inbox_frame, "Review Requests", &inbox_full);

                let issues_frame = common::frame_from_capture(&report.captures[2]);
                let issues_full = Region::full(issues_frame.cols(), issues_frame.rows());
                assertion::assert_text_in_region(&issues_frame, "Issues", &issues_full);

                let settings_frame = common::frame_from_capture(&report.captures[3]);
                let settings_full = Region::full(settings_frame.cols(), settings_frame.rows());
                assertion::assert_text_in_region(
                    &settings_frame,
                    "Default Smart Model",
                    &settings_full,
                );
            },
        )?;

    Ok(())
}

/// Verify that the Inbox tab renders requested PR/MR review state at a
/// width that leaves grouped review headings and titles inspectable.
#[test]
fn inbox_tab_shows_requested_reviews_page() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("inbox_tab")
        .with_git()
        .with_terminal_size(120, 36)
        .setup(seed_inbox_tab_requested_reviews)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Inbox"))
                    .wait_for_text("Requested from you", 5000)
                    .wait_for_text("Requested from your groups", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("inbox", "Inbox tab selected")
                    .press_key("Enter")
                    .wait_for_text("Review Request", 5000)
                    .wait_for_text("Loading comments...", 5000)
                    .capture_labeled("loading", "Review request detail while comments load")
                    .wait_for_text("Personal parser review description.", 5000)
                    .wait_for_text("Release notes", 5000)
                    .wait_for_text("General discussion", 5000)
                    .wait_for_text("Please show this selected review comment.", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("detail", "Review request detail after Enter")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Review Request", &full);
                assertion::assert_text_in_region(frame, "Title", &full);
                assertion::assert_text_in_region(frame, "Review personal parser", &full);
                assertion::assert_text_in_region(frame, "Author", &full);
                assertion::assert_text_in_region(frame, "octocat", &full);
                assertion::assert_text_in_region(frame, "Description", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Personal parser review description.",
                    &full,
                );
                assertion::assert_text_in_region(frame, "Release notes", &full);
                assertion::assert_text_in_region(frame, "v1.0.0", &full);
                assertion::assert_text_in_region(frame, "Fix parser output.", &full);
                assertion::assert_text_in_region(frame, "Comments", &full);
                assertion::assert_text_in_region(frame, "General discussion", &full);
                assertion::assert_text_in_region(frame, "General review comment.", &full);
                assertion::assert_text_in_region(
                    frame,
                    "crates/agentty/src/ui/page/review_detail.rs:7",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "Please show this selected review comment.",
                    &full,
                );

                let inbox_frame = common::frame_from_capture(&report.captures[0]);
                let inbox_full = Region::full(inbox_frame.cols(), inbox_frame.rows());
                assertion::assert_text_in_region(&inbox_frame, "Review Requests", &inbox_full);
                assertion::assert_text_in_region(&inbox_frame, "Requested from you", &inbox_full);
                assertion::assert_text_in_region(
                    &inbox_frame,
                    "PR #42 Review personal parser",
                    &inbox_full,
                );
                assertion::assert_text_in_region(&inbox_frame, "octocat", &inbox_full);
                assertion::assert_text_in_region(
                    &inbox_frame,
                    "Requested from your groups",
                    &inbox_full,
                );
                assertion::assert_text_in_region(
                    &inbox_frame,
                    "PR #43 Review team parser",
                    &inbox_full,
                );
                assertion::assert_text_in_region(&inbox_frame, "team-lead", &inbox_full);

                let loading_frame = common::frame_from_capture(&report.captures[1]);
                let loading_full = Region::full(loading_frame.cols(), loading_frame.rows());
                assertion::assert_text_in_region(
                    &loading_frame,
                    "Loading comments...",
                    &loading_full,
                );
            },
        )?;

    Ok(())
}

/// Verify the Issues tab loads base details for a selected assigned issue.
#[test]
fn test_assigned_github_issues() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("assigned_github_issues")
        .with_git()
        .with_terminal_size(120, 36)
        .setup(seed_assigned_github_issues)
        .zola(
            "Assigned GitHub issues",
            "Browse assigned GitHub issues and inspect their base details.",
            45,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Inbox"))
                    .compose(&common::switch_to_tab("Issues"))
                    .wait_for_text("Keep issue list compact", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("issues", "Assigned GitHub issue list")
                    .press_key("Enter")
                    .wait_for_text("Description", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("issue_detail", "Selected GitHub issue details")
            },
            |frame, report| {
                assert_eq!(report.captures.len(), 2);
                let issues_frame = common::frame_from_capture(&report.captures[0]);
                let issues_full = Region::full(issues_frame.cols(), issues_frame.rows());
                assertion::assert_text_in_region(&issues_frame, "Issues", &issues_full);
                assertion::assert_text_in_region(&issues_frame, "Assigned to you", &issues_full);
                assertion::assert_text_in_region(
                    &issues_frame,
                    "#124 Keep issue list compact",
                    &issues_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Issue #124", &full);
                assertion::assert_text_in_region(frame, "Author: hubot", &full);
                assertion::assert_text_in_region(frame, "Assignees: octocat", &full);
                assertion::assert_text_in_region(frame, "Labels: enhancement, ui", &full);
                assertion::assert_text_in_region(frame, "agentty-xyz/agentty", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Show the selected issue's base information",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify an assigned GitHub issue can start a session with its link.
#[test]
fn test_address_assigned_github_issue() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("address_assigned_github_issue")
        .with_git()
        .with_terminal_size(120, 36)
        .setup(seed_assigned_github_issues)
        .zola(
            "Address an assigned GitHub issue",
            "Start an agent session from an assigned issue with its link included.",
            46,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Inbox"))
                    .compose(&common::switch_to_tab("Issues"))
                    .wait_for_text("Keep issue list compact", 5000)
                    .press_key("Enter")
                    .wait_for_text("Description", 5000)
                    .capture_labeled("issue_detail", "Issue detail with address action")
                    .press_key("a")
                    .wait_for_text(
                        "Address this issue: https://github.com/agentty-xyz/agentty/issues/124",
                        10000,
                    )
                    .viewing_pause_ms(1500)
                    .capture_labeled("issue_session", "New session addressing the issue")
            },
            |frame, report| {
                assert_eq!(report.captures.len(), 2);
                let issue_frame = common::frame_from_capture(&report.captures[0]);
                let issue_full = Region::full(issue_frame.cols(), issue_frame.rows());
                assertion::assert_text_in_region(&issue_frame, "a: address", &issue_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Address this issue: https://github.com/agentty-xyz/agentty/issues/124",
                    &full,
                );
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
/// Starts on Projects (first tab), then presses `BackTab` to cycle back
/// through Settings, Issues, Inbox, Sessions, and Projects.
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
                    .compose(&common::switch_to_tab_reverse("Settings"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_settings", "Settings tab after first BackTab")
                    .compose(&common::switch_to_tab_reverse("Issues"))
                    .viewing_pause_ms(1500)
                    .capture_labeled("back_to_issues", "Issues tab after second BackTab")
                    .compose(&common::switch_to_tab_reverse("Inbox"))
                    .viewing_pause_ms(1500)
                    .capture_labeled("back_to_inbox", "Inbox tab after third BackTab")
                    .compose(&common::switch_to_tab_reverse("Sessions"))
                    .viewing_pause_ms(1500)
                    .capture_labeled("back_to_sessions", "Sessions tab after fourth BackTab")
                    .compose(&common::switch_to_tab_reverse("Projects"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_projects", "Projects tab after fifth BackTab")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "test-project", &full);

                let settings_frame = common::frame_from_capture(&report.captures[0]);
                let settings_full = Region::full(settings_frame.cols(), settings_frame.rows());
                assertion::assert_text_in_region(
                    &settings_frame,
                    "Default Smart Model",
                    &settings_full,
                );

                let issues_frame = common::frame_from_capture(&report.captures[1]);
                let issues_full = Region::full(issues_frame.cols(), issues_frame.rows());
                assertion::assert_text_in_region(&issues_frame, "Issues", &issues_full);

                let inbox_frame = common::frame_from_capture(&report.captures[2]);
                let inbox_full = Region::full(inbox_frame.cols(), inbox_frame.rows());
                assertion::assert_text_in_region(&inbox_frame, "Review Requests", &inbox_full);

                let sessions_frame = common::frame_from_capture(&report.captures[3]);
                let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
                assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

                let projects_frame = common::frame_from_capture(&report.captures[4]);
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
