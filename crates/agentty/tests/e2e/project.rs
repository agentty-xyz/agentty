//! Projects page E2E tests: project dashboard activity and summary data.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use ag_session::SettingName;
use testty::assertion;
use testty::proof::report::ProofReport;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

#[tokio::test]
async fn test_relative_agentty_root_excludes_managed_worktrees()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange, Act, Assert
    FeatureTest::new("relative_agentty_root_excludes_managed_worktrees")
        .with_git()
        .env("AGENTTY_ROOT", "managed-root")
        .setup(|env| {
            Box::pin(async move {
                let root = env.workdir.join("managed-root");
                std::fs::create_dir_all(root.join("wt/session/.git"))?;
                let database = agentty::db::Database::open(&root.join("db/agentty.db")).await?;
                database
                    .settings()
                    .upsert_setting(SettingName::ProjectDiscoveryPromptSeen, "true")
                    .await?;
                Ok(())
            })
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .wait_for_text("Add project", 5000)
                    .write_text("~/test-project/managed-root/wt/session")
                    .press_key("Enter")
                    .wait_for_text("session worktrees", 5000)
                    .capture_labeled("rejected", "Managed worktree cannot be registered")
                    .press_key("Esc")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("d")
                    .wait_for_text("Discover projects in folder", 5000)
                    .write_text("~/test-project")
                    .press_key("Enter")
                    .wait_for_text("Select repositories to add", 10000)
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "test-project", &full);
                    assertion::assert_not_visible(frame, "wt/session");
                    let capture = report
                        .captures
                        .iter()
                        .find(|capture| capture.label == "rejected")
                        .expect("rejection capture");
                    let rejection = common::frame_from_capture(capture);
                    assertion::assert_text_in_region(&rejection, "session worktrees", &full);
                })
            },
        )
        .await?;

    Ok(())
}

/// Emulates a database created by launching an older version in a subdirectory.
async fn seed_legacy_subdirectory(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = env.workdir.join("legacy-subdir");
    std::fs::create_dir_all(&legacy)?;
    let database = common::open_database(env).await?;
    let legacy_id = database
        .projects()
        .upsert_project(&legacy.to_string_lossy(), None)
        .await?;
    database
        .sessions()
        .insert_session(
            "legacy-session",
            "claude-haiku-4-5-20251001",
            "main",
            "Review",
            legacy_id,
        )
        .await?;
    database
        .sessions()
        .update_session_title("legacy-session", "Legacy history")
        .await?;
    database
        .settings()
        .upsert_project_setting(legacy_id, SettingName::LaunchConfiguration, "legacy-launch")
        .await?;
    std::fs::create_dir_all(
        crate::test_support::session_folder(&env.agentty_root.join("wt"), "legacy-session")
            .join(".agentty"),
    )?;

    Ok(())
}

#[tokio::test]
async fn test_upgrade_opens_legacy_subdirectory_history() -> Result<(), Box<dyn std::error::Error>>
{
    // Arrange, Act, Assert
    FeatureTest::new("upgrade_opens_legacy_subdirectory_history")
        .with_git()
        .setup(|env| Box::pin(seed_legacy_subdirectory(env)))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("legacy-subdir", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Legacy history", 5000)
                    .capture_labeled("history", "Legacy project sessions remain accessible")
                    .compose(&common::switch_to_tab("Settings"))
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "legacy-launch", &full);
                    assertion::assert_text_in_region(frame, "legacy-subdir", &full);
                    let capture = report
                        .captures
                        .iter()
                        .find(|capture| capture.label == "history")
                        .expect("history capture");
                    let history = common::frame_from_capture(capture);
                    assertion::assert_text_in_region(&history, "Legacy history", &full);
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_non_repository_launch_stays_out_of_project_switcher()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange, Act, Assert
    FeatureTest::new("non_repository_launch_stays_out_of_project_switcher")
        .setup(|env| {
            Box::pin(async move {
                std::fs::create_dir_all(env.home_dir.join("added-repo/.git"))?;
                Ok(())
            })
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .wait_for_text("Add project", 5000)
                    .write_text("~/added-repo")
                    .press_key("Enter")
                    .wait_for_text("added-repo", 5000)
                    .press_key("Enter")
                    .wait_for_text("Project: added-repo", 5000)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "added-repo", &full);
                    assertion::assert_not_visible(frame, "test-project");
                })
            },
        )
        .await?;

    Ok(())
}

/// Configures two local upstreams and delays `git pull` long enough to prove
/// navigation and cross-project sync queueing remain non-modal.
async fn seed_delayed_project_sync(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_second_project(env).await?;

    let origin = env.agentty_root.join("project-sync-origin.git");
    configure_local_upstream(&env.workdir, &origin)?;
    let second_project_dir = env
        .workdir
        .parent()
        .ok_or("missing temp root for second project")?
        .join("zeta-project");
    let second_origin = env.agentty_root.join("second-project-sync-origin.git");
    configure_local_upstream(&second_project_dir, &second_origin)?;

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let real_git = real_git.to_string_lossy().replace('\'', "'\"'\"'");
    let git_path = env.stub_bin.join("git");
    std::fs::write(
        &git_path,
        format!(
            r#"#!/bin/sh
if [ "$1" = "pull" ]; then
  sleep 4
fi
exec '{real_git}' "$@"
"#
        ),
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Creates a bare local remote and publishes one test project's `main` branch.
fn configure_local_upstream(
    project_dir: &Path,
    origin: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(origin)?;
    run_git(origin, &["init", "--bare", "."])?;
    let origin_path = origin.to_string_lossy().into_owned();
    run_git(
        project_dir,
        &["remote", "add", "origin", origin_path.as_str()],
    )?;
    run_git(project_dir, &["push", "--set-upstream", "origin", "main"])?;

    Ok(())
}

/// Runs one Git command with deterministic test identity.
fn run_git(working_directory: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("git")
        .args(args)
        .current_dir(working_directory)
        .env("GIT_AUTHOR_NAME", "Agentty Test")
        .env("GIT_AUTHOR_EMAIL", "agentty@example.com")
        .env("GIT_COMMITTER_NAME", "Agentty Test")
        .env("GIT_COMMITTER_EMAIL", "agentty@example.com")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("git {} failed with {status}", args.join(" ")).into());
    }

    Ok(())
}

/// Verifies that session creation is rejected in-app during project sync.
fn verify_session_creation_blocked(report: &ProofReport) -> Result<(), &'static str> {
    let capture = report
        .captures
        .iter()
        .find(|capture| capture.label == "session_creation_blocked")
        .ok_or("missing blocked session creation capture")?;
    let frame = common::frame_from_capture(capture);
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "Session creation unavailable", &full);
    assertion::assert_text_in_region(&frame, "is synchronizing main", &full);

    Ok(())
}

/// Verify that the Projects tab lists the registered git project name and
/// branch from the temp workdir and shows dashboard activity plus work stats.
///
/// Agentty auto-registers the current git working directory as a project on
/// startup. The test creates a `test-project` repository and asserts that
/// the project name appears in the project list.
#[tokio::test]
async fn projects_page_shows_cwd() {
    // Arrange, Act, Assert
    FeatureTest::new("projects_cwd")
        .with_git()
        .setup(|env| Box::pin(common::seed_projects_startup_tab(env)))
        .zola(
            "Project dashboard",
            "See project activity and switch between registered projects.",
            90,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("gemini 0.0.1-updated", 10000)
                    .wait_for_text("claude 0.0.1-updated", 10000)
                    .wait_for_text("codex 0.0.1-updated", 10000)
                    .viewing_pause_ms(3000)
                    .capture_labeled("projects", "Projects page with registered project")
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Activity", &full);
                    assertion::assert_text_in_region(frame, "Branch", &full);
                    assertion::assert_text_in_region(frame, "Sessions", &full);
                    assertion::assert_text_in_region(frame, "Work Pace", &full);
                    assertion::assert_text_in_region(frame, "Agent CLIs", &full);
                    assertion::assert_text_in_region(frame, "claude", &full);
                    assertion::assert_text_in_region(frame, "0.0.1-updated", &full);
                    assertion::assert_text_in_region(frame, "gemini 0.0.1-updated", &full);
                    assertion::assert_text_in_region(frame, "Tokens In", &full);
                    assertion::assert_text_in_region(frame, "Out", &full);
                    assertion::assert_not_visible(frame, "Version");
                    assertion::assert_not_visible(frame, "Agentty is an ADE");
                    assertion::assert_not_visible(frame, "Last Opened");
                    assertion::assert_text_in_region(frame, "Active", &full);
                    assertion::assert_text_in_region(frame, "test-project", &full);
                    assertion::assert_text_in_region(frame, "main", &full);
                })
            },
        )
        .await
        .expect("feature test failed");
}

/// Verify that `p` on the Sessions tab opens the MRU-ordered project switcher
/// popup and that selecting another project switches the active project
/// without leaving the Sessions view.
///
/// The test seeds a second registered project (`zeta-project`) that was never
/// opened, so the active `test-project` stays first in MRU order and the
/// seeded project sorts below it even when both projects receive the same
/// pinned last-opened timestamp. The scenario starts on a pre-persisted
/// Sessions tab and toggles the active project there and back, so the follow-up
/// VHS recording replays against the same MRU order as the assertion run.
#[tokio::test]
async fn test_project_switcher() {
    // Arrange, Act, Assert
    FeatureTest::new("project_switcher")
        .with_git()
        .setup(|env| Box::pin(async move { common::seed_second_project(env).await }))
        .zola(
            "Project switcher",
            "Switch the active project from the Sessions view with a quick MRU popup.",
            91,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(1500)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("switcher", "MRU project switcher popup")
                    .press_key("j")
                    .wait_for_stable_frame(300, 3000)
                    .viewing_pause_ms(1000)
                    .press_key("Enter")
                    .wait_for_text("Project: zeta-project", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("switched", "Sessions view scoped to the switched project")
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .press_key("Enter")
                    .wait_for_text("Project: test-project", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled(
                        "switched_back",
                        "Sessions view restored to the first project",
                    )
            },
            |frame, report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "p: projects", &full);
                    assertion::assert_text_in_region(frame, "Project: test-project", &full);
                    assertion::assert_not_visible(frame, "Switch project");

                    let popup_capture = report
                        .captures
                        .iter()
                        .find(|capture| capture.label == "switcher")
                        .expect("missing switcher capture");
                    let popup_frame = common::frame_from_capture(popup_capture);
                    let popup_region = Region::full(popup_frame.cols(), popup_frame.rows());
                    assertion::assert_text_in_region(&popup_frame, "Switch project", &popup_region);
                    assertion::assert_text_in_region(&popup_frame, "* test-project", &popup_region);
                    assertion::assert_text_in_region(&popup_frame, "zeta-project", &popup_region);

                    let switched_capture = report
                        .captures
                        .iter()
                        .find(|capture| capture.label == "switched")
                        .expect("missing switched capture");
                    let switched_frame = common::frame_from_capture(switched_capture);
                    let switched_region =
                        Region::full(switched_frame.cols(), switched_frame.rows());
                    assertion::assert_text_in_region(
                        &switched_frame,
                        "Project: zeta-project",
                        &switched_region,
                    );
                })
            },
        )
        .await
        .expect("feature test failed");
}

/// Verify explicit project sync reports progress in the status bar while tab
/// navigation remains available, then runs a queued sync for another project.
#[tokio::test]
async fn test_project_sync_non_modal() {
    // Arrange, Act, Assert
    FeatureTest::new("project_sync_non_modal")
        .with_git()
        .setup(|env| Box::pin(async move { seed_delayed_project_sync(env).await }))
        .zola(
            "Non-modal project sync",
            "Keep navigating while Agentty safely synchronizes the project branch.",
            92,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("s")
                    .wait_for_text("Syncing test-project/main...", 5000)
                    .capture_labeled(
                        "syncing_while_navigating",
                        "Sessions tab remains interactive during project sync",
                    )
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Session creation unavailable", 5000)
                    .capture_labeled(
                        "session_creation_blocked",
                        "Session creation is safely blocked without exiting Agentty",
                    )
                    .press_key("Enter")
                    .wait_for_text("Sessions", 5000)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Project: zeta-project", 5000)
                    .press_key("s")
                    .wait_for_text("Syncing zeta-project/main...", 15_000)
                    .capture_labeled(
                        "queued_project_sync",
                        "Second project sync starts after the first finishes",
                    )
                    .wait_for_text("Synced zeta-project/main", 15_000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "sync_complete",
                        "Project sync completes in the status bar without a popup",
                    )
                    .sleep_ms(10_500)
                    .capture_labeled(
                        "sync_status_expired",
                        "The status bar returns to its normal page hint",
                    )
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    assert_project_sync_non_modal(frame, report)
                        .expect("feature captures should exist");
                })
            },
        )
        .await
        .expect("feature test failed");
}

/// Verify a staged draft cannot materialize its worktree while project sync
/// owns the base checkout.
#[tokio::test]
async fn project_sync_blocks_staged_draft_start() {
    // Arrange, Act, Assert
    FeatureTest::new("project_sync_blocks_staged_draft_start")
        .with_git()
        .setup(|env| Box::pin(async move { seed_delayed_project_sync(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 5000)
                    .write_text("Start after project sync")
                    .press_key("Enter")
                    .wait_for_text("s: start", 5000)
                    .press_key("q")
                    .wait_for_text("new session", 5000)
                    .press_key("s")
                    .wait_for_text("Syncing test-project/main...", 5000)
                    .press_key("Enter")
                    .wait_for_text("Draft Session", 5000)
                    .press_key("s")
                    .wait_for_text("[Start Error]", 5000)
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "[Start Error]", &full);
                    assertion::assert_text_in_region(frame, "is synchronizing", &full);
                    assertion::assert_text_in_region(frame, "s: start", &full);
                })
            },
        )
        .await
        .expect("feature test failed");
}

/// Checks the captured states for this feature journey.
fn assert_project_sync_non_modal(
    frame: &testty::frame::TerminalFrame,
    report: &testty::proof::report::ProofReport,
) -> Result<(), &'static str> {
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_not_visible(frame, "Synced zeta-project/main");
    assertion::assert_text_in_region(frame, "FYI:", &full);
    assertion::assert_not_visible(frame, "Sync complete");

    let complete_capture = report
        .captures
        .iter()
        .find(|capture| capture.label == "sync_complete")
        .ok_or("missing completed sync capture")?;
    let complete_frame = common::frame_from_capture(complete_capture);
    let complete_full = Region::full(complete_frame.cols(), complete_frame.rows());
    assertion::assert_text_in_region(&complete_frame, "Synced zeta-project/main", &complete_full);

    let syncing_capture = report
        .captures
        .iter()
        .find(|capture| capture.label == "syncing_while_navigating")
        .ok_or("missing in-progress sync capture")?;
    let syncing_frame = common::frame_from_capture(syncing_capture);
    let syncing_full = Region::full(syncing_frame.cols(), syncing_frame.rows());
    assertion::assert_text_in_region(
        &syncing_frame,
        "Syncing test-project/main...",
        &syncing_full,
    );
    assertion::assert_text_in_region(&syncing_frame, "Sessions", &syncing_full);
    assertion::assert_not_visible(&syncing_frame, "Sync in progress");

    verify_session_creation_blocked(report)?;

    let queued_capture = report
        .captures
        .iter()
        .find(|capture| capture.label == "queued_project_sync")
        .ok_or("missing queued project sync capture")?;
    let queued_frame = common::frame_from_capture(queued_capture);
    let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
    assertion::assert_text_in_region(&queued_frame, "Syncing zeta-project/main...", &queued_full);

    Ok(())
}

/// Enables the real first-run invitation without changing other PTY fixtures.
async fn seed_discovery_invitation(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::open_database(env)
        .await?
        .settings()
        .upsert_setting(ag_session::SettingName::ProjectDiscoveryPromptSeen, "false")
        .await?;
    std::fs::create_dir_all(env.home_dir.join("discover-me/.git"))?;

    Ok(())
}

#[tokio::test]
async fn test_project_discovery_startup_skip() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange, Act, Assert
    FeatureTest::new("project_discovery_startup_skip")
        .with_terminal_size(120, 40)
        .with_git()
        .setup(|env| Box::pin(seed_discovery_invitation(env)))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Discover repositories?", 5000)
                    .capture_labeled("invitation", "Home discovery requires consent")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_not_visible(frame, "Discover repositories?");
                    assertion::assert_not_visible(frame, "discover-me");
                    let capture = report
                        .captures
                        .iter()
                        .find(|capture| capture.label == "invitation")
                        .expect("invitation capture");
                    let invitation = common::frame_from_capture(capture);
                    assertion::assert_text_in_region(&invitation, "macOS folder-access", &full);
                    assertion::assert_text_in_region(&invitation, "prompts.", &full);
                    assertion::assert_text_in_region(&invitation, "Yes", &full);
                    assertion::assert_text_in_region(&invitation, "No", &full);
                    assertion::assert_not_visible(&invitation, "Enter: confirm");
                    assertion::assert_not_visible(&invitation, "n/Esc: skip");
                    let title = invitation.find_text("Discover repositories?")[0].rect;
                    let border = invitation.row_text(title.row);
                    let left = border
                        .chars()
                        .position(|cell| cell == '╭')
                        .expect("left border");
                    let right = border
                        .chars()
                        .position(|cell| cell == '╮')
                        .expect("right border");
                    assert_eq!(right - left + 1, 70);
                    assert_eq!(left, 25);
                    let question = invitation.find_text("Discover Git repositories")[0].rect;
                    assert_eq!(question.row, title.row + 3, "two blank rows above content");
                    for row in title.row + 1..question.row {
                        assert!((26..94).all(|column| invitation.cell_text(row, column) == " "));
                    }
                    let path_row = question.row + 1;
                    assert!(
                        invitation
                            .cell_style(path_row, 28)
                            .expect("path style")
                            .bold()
                    );
                    assert_eq!(
                        invitation.fg_color(path_row, 28),
                        invitation.fg_color(title.row, title.col)
                    );
                    let no = invitation
                        .find_text("No")
                        .into_iter()
                        .find(|choice| choice.style.bold())
                        .expect("selected No choice");
                    assert_eq!(invitation.cell_text(no.rect.row + 2, 25), "╰");
                    assert!(no.rect.row + 3 - title.row < 20, "popup fits its content");
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_project_discovery_startup_short_viewport() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange, Act, Assert
    FeatureTest::new("project_discovery_startup_short_viewport")
        .with_terminal_size(80, 11)
        .with_git()
        .setup(|env| Box::pin(seed_discovery_invitation(env)))
        .run(
            |scenario| {
                scenario
                    .wait_for_text("Discover repositories?", 30000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "short_invitation",
                        "Choices remain visible in a short terminal",
                    )
                    .press_key("Esc")
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    assertion::assert_not_visible(frame, "Discover repositories?");
                    let capture = report
                        .captures
                        .iter()
                        .find(|capture| capture.label == "short_invitation")
                        .expect("short invitation capture");
                    let invitation = common::frame_from_capture(capture);
                    let full = Region::full(invitation.cols(), invitation.rows());
                    for expected in ["Yes", "No"] {
                        assertion::assert_text_in_region(&invitation, expected, &full);
                    }
                    assert!(
                        invitation
                            .find_text("No")
                            .iter()
                            .any(|choice| choice.style.bold())
                    );
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_project_discovery_startup_accept_and_select() -> Result<(), Box<dyn std::error::Error>>
{
    // Arrange, Act, Assert
    FeatureTest::new("project_discovery_startup_accept_and_select")
        .with_git()
        .setup(|env| Box::pin(seed_discovery_invitation(env)))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Discover repositories?", 5000)
                    .press_key("y")
                    .wait_for_text("Select repositories to add", 10000)
                    .capture_labeled("results", "No repositories are selected automatically")
                    .press_key("Space")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "discover-me", &full);
                    assertion::assert_not_visible(frame, "Select repositories to add");
                    let capture = report
                        .captures
                        .iter()
                        .find(|capture| capture.label == "results")
                        .expect("results capture");
                    let results = common::frame_from_capture(capture);
                    assertion::assert_text_in_region(&results, "[ ]", &full);
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_project_discovery_add_and_chosen_folder() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange, Act, Assert
    FeatureTest::new("project_discovery_add_and_chosen_folder")
        .with_git()
        .setup(|env| {
            Box::pin(async move {
                std::fs::create_dir_all(env.home_dir.join("added-repo/.git"))?;
                std::fs::create_dir_all(env.home_dir.join("chosen/found-repo/.git"))?;
                std::fs::create_dir_all(env.home_dir.join("outside-scope/.git"))?;
                Ok(())
            })
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .wait_for_text("Add project", 5000)
                    .write_text("~/added-repo")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .wait_for_text("added-repo", 5000)
                    .press_key("d")
                    .wait_for_text("Discover projects in folder", 5000)
                    .write_text("~/chosen")
                    .press_key("Enter")
                    .wait_for_text("Select repositories to add", 10000)
                    .press_key("Space")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "added-repo", &full);
                    assertion::assert_text_in_region(frame, "found-repo", &full);
                    assertion::assert_not_visible(frame, "outside-scope");
                })
            },
        )
        .await?;

    Ok(())
}
