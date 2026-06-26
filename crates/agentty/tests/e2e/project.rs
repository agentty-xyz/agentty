//! Projects page E2E tests: project dashboard activity and summary data.

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

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
