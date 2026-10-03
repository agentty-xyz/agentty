//! Archive pagination through the real terminal and repository.

use testty::assertion;
use testty::region::Region;

use super::fixture::E2eResult;
use crate::common;
use crate::common::{FeatureTest, SessionSeed};

#[tokio::test]
async fn session_archive_load_more_pages() -> E2eResult {
    // Arrange
    FeatureTest::new("session_archive_load_more")
        .with_git()
        .with_terminal_size(120, 40)
        .setup(|env| {
            Box::pin(async move {
                common::seed_active_project_setting(env).await?;
                common::seed_session(
                    env,
                    SessionSeed::draft("active", "gpt-5.6-sol", "main", "Draft")
                        .with_title("active-visible"),
                )
                .await?;
                let database = common::open_database(env).await?;
                for index in 0..23 {
                    let id = format!("archive-{index:02}");
                    common::seed_session(
                        env,
                        SessionSeed::regular(
                            &id,
                            "gpt-5.6-sol",
                            "main",
                            if index % 2 == 0 { "Done" } else { "Canceled" },
                        )
                        .with_title(&id),
                    )
                    .await?;
                    database
                        .sessions()
                        .update_session_updated_at(&id, 100 - index)
                        .await?;
                }

                Ok(())
            })
        })
        .run(
            |scenario| {
                // Act
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Load more...", 5000)
                    .capture_labeled("initial", "First ten archived sessions")
                    .press_key("k")
                    .wait_for_text("Enter: load more", 5000)
                    .press_key("Enter")
                    .wait_for_text("archive-19", 5000)
                    .capture_labeled("second", "Twenty archived sessions after Enter");
                let mut scenario = scenario;
                for _ in 0..10 {
                    scenario = scenario.press_key("j");
                }
                scenario
                    .wait_for_text("Enter: load more", 5000)
                    .press_key("Enter")
                    .wait_for_text("archive-22", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("last", "Remaining archive without pagination action")
                    .press_key("Enter")
                    .wait_for_text("q: back", 5000)
                    .capture_labeled("opened", "Newly loaded archive session can be opened")
            },
            |_frame, report| {
                Box::pin(async move {
                    // Assert
                    let initial = common::frame_from_capture(&report.captures[0]);
                    let second = common::frame_from_capture(&report.captures[1]);
                    let last = common::frame_from_capture(&report.captures[2]);
                    for (frame, expected, hidden) in [
                        (&initial, "archive-09", "archive-10"),
                        (&second, "archive-19", "archive-20"),
                    ] {
                        let full = Region::full(frame.cols(), frame.rows());
                        assertion::assert_text_in_region(frame, "active-visible", &full);
                        assertion::assert_text_in_region(frame, expected, &full);
                        assertion::assert_text_in_region(frame, "Load more...", &full);
                        assertion::assert_not_visible(frame, hidden);
                    }
                    let full = Region::full(last.cols(), last.rows());
                    assertion::assert_text_in_region(&last, "archive-22", &full);
                    assertion::assert_text_in_region(&last, "active-visible", &full);
                    assertion::assert_not_visible(&last, "Load more...");
                })
            },
        )
        .await?;

    Ok(())
}
