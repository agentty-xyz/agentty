//! Projects page E2E tests: project dashboard activity and summary data.

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

/// Verify that the Projects tab lists the registered git project name from
/// the temp workdir and shows dashboard-level activity.
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
                assertion::assert_text_in_region(frame, "Agentty", &full);
                assertion::assert_text_in_region(frame, "Activity Heatmap", &full);
                assertion::assert_text_in_region(frame, "Sessions", &full);
                assertion::assert_text_in_region(frame, "Last Opened", &full);
                assertion::assert_text_in_region(frame, "test-project", &full);
                assertion::assert_not_visible(frame, "Branch");
            },
        )
        .expect("feature test failed");
}
