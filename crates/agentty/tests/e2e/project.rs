//! Projects page E2E tests: project dashboard activity and summary data.

use std::path::Path;

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Verify that the Projects tab lists the registered git project name and
/// branch from the temp workdir and shows dashboard activity plus work stats.
///
/// Agentty auto-registers the current git working directory as a project on
/// startup. The test creates a `test-project` repository and asserts that
/// the project name appears in the project list.
#[test]
fn projects_page_shows_cwd() {
    // Arrange, Act, Assert
    FeatureTest::new("projects_cwd")
        .with_git()
        .zola(
            "Project dashboard",
            "See project activity and switch between registered projects.",
            90,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(3000)
                    .capture_labeled("projects", "Projects page with registered project")
            },
            |frame, _report| {
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
            },
        )
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
#[test]
fn test_project_switcher() {
    // Arrange, Act, Assert
    FeatureTest::new("project_switcher")
        .with_git()
        .setup(seed_second_project)
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
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("switched", "Sessions view scoped to the switched project")
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled(
                        "switched_back",
                        "Sessions view restored to the first project",
                    )
            },
            |frame, report| {
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
                let switched_region = Region::full(switched_frame.cols(), switched_frame.rows());
                assertion::assert_text_in_region(
                    &switched_frame,
                    "Project: zeta-project",
                    &switched_region,
                );
            },
        )
        .expect("feature test failed");
}

/// Seed one additional never-opened git project so the switcher popup lists
/// two registered projects, and start the app on the Sessions tab.
fn seed_second_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let temp_root = env
        .workdir
        .parent()
        .ok_or("missing temp root for second project")?;
    let second_project_dir = temp_root.join("zeta-project");
    std::fs::create_dir_all(&second_project_dir)?;
    init_git_repository(&second_project_dir)?;

    let second_project_path = second_project_dir.canonicalize()?;
    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .projects()
            .upsert_project(
                &second_project_path.to_string_lossy(),
                Some("main".to_string()),
            )
            .await?;
        agentty::test_support::persist_active_tab_for_test(&database, agentty::app::Tab::Sessions)
            .await?;

        Ok::<(), agentty::db::DbError>(())
    })?;

    Ok(())
}

/// Initialize a `main`-branch git repository with one empty commit in `dir`.
fn init_git_repository(dir: &Path) -> std::io::Result<()> {
    let run = |args: &[&str]| -> std::io::Result<()> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(())
    };
    run(&["init", "-b", "main"])?;
    run(&["config", "user.email", "test@test.com"])?;
    run(&["config", "user.name", "Test"])?;
    run(&["commit", "--allow-empty", "-m", "init"])
}
