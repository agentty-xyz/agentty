//! Archive pagination and collapse through the real terminal and repository.

use testty::assertion;
use testty::proof::report::ProofReport;
use testty::region::Region;

use super::fixture::E2eResult;
use crate::common;
use crate::common::{FeatureTest, SessionSeed};

#[tokio::test]
async fn session_archive_load_more_and_show_less_pages() -> E2eResult {
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
                let mut scenario = scenario
                    .wait_for_text("Enter: load more", 5000)
                    .press_key("Enter")
                    .wait_for_text("archive-22", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("last", "Remaining archive with only the show-less action")
                    .press_key("Enter")
                    .wait_for_text("q: back", 5000)
                    .capture_labeled("opened", "Newly loaded archive session can be opened")
                    .press_key("q")
                    .wait_for_text("Show less...", 5000);
                for _ in 0..3 {
                    scenario = scenario.press_key("j");
                }
                scenario
                    .wait_for_text("Enter: show less", 5000)
                    .press_key("Enter")
                    .wait_for_text("Load more...", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("collapsed", "Show less returns to the first ten")
            },
            |_frame, report| {
                Box::pin(async move {
                    // Assert
                    assert_archive_load_more_and_show_less(report);
                })
            },
        )
        .await?;

    Ok(())
}

/// Verifies archive pages expand with `Load more...` and collapse with
/// `Show less...`.
fn assert_archive_load_more_and_show_less(report: &ProofReport) {
    let initial = common::frame_from_capture(&report.captures[0]);
    let second = common::frame_from_capture(&report.captures[1]);
    let last = common::frame_from_capture(&report.captures[2]);
    let collapsed = common::frame_from_capture(&report.captures[4]);
    for (frame, expected, hidden) in [
        (&initial, "archive-09", "archive-10"),
        (&second, "archive-19", "archive-20"),
        (&collapsed, "archive-09", "archive-10"),
    ] {
        let full = Region::full(frame.cols(), frame.rows());
        assertion::assert_text_in_region(frame, "active-visible", &full);
        assertion::assert_text_in_region(frame, "ARCHIVE —— 23", &full);
        assertion::assert_text_in_region(frame, "ACTIVE —— 1", &full);
        assertion::assert_text_in_region(frame, expected, &full);
        assertion::assert_text_in_region(frame, "Load more...", &full);
        assertion::assert_not_visible(frame, hidden);
    }
    let full = Region::full(second.cols(), second.rows());
    assertion::assert_text_in_region(&second, "Show less...", &full);
    assertion::assert_not_visible(&initial, "Show less...");
    assertion::assert_not_visible(&collapsed, "Show less...");
    assertion::assert_text_in_region(&collapsed, "Enter: load more", &full);
    let full = Region::full(last.cols(), last.rows());
    assertion::assert_text_in_region(&last, "archive-22", &full);
    assertion::assert_text_in_region(&last, "active-visible", &full);
    assertion::assert_text_in_region(&last, "ARCHIVE —— 23", &full);
    assertion::assert_text_in_region(&last, "Show less...", &full);
    assertion::assert_not_visible(&last, "Load more...");
}

#[tokio::test]
async fn session_archive_total_survives_skipped_permission_modes() -> E2eResult {
    // Arrange
    for archive_count in [2, 11] {
        FeatureTest::new(format!("session_archive_skipped_modes_{archive_count}"))
            .with_git()
            .setup(move |env| {
                Box::pin(async move {
                    common::seed_active_project_setting(env).await?;
                    let database = common::open_database(env).await?;
                    for index in 0..archive_count {
                        let id = format!("archive-{index:02}");
                        common::seed_session(
                            env,
                            SessionSeed::regular(&id, "gpt-5.6-sol", "main", "Done")
                                .with_title(&id),
                        )
                        .await?;
                        database
                            .sessions()
                            .update_session_updated_at(&id, 100 - index)
                            .await?;
                        if index < 10 {
                            sqlx::query(
                                "UPDATE session SET permission_mode = 'unsupported' WHERE id = ?",
                            )
                            .bind(&id)
                            .execute(database.pool())
                            .await?;
                        }
                    }

                    Ok(())
                })
            })
            .run(
                move |scenario| {
                    // Act
                    let scenario = scenario
                        .compose(&common::wait_for_agentty_startup())
                        .wait_for_text(format!("ARCHIVE —— {archive_count}"), 5000)
                        .capture_labeled("skipped", "Archive total without loadable rows");
                    if archive_count > 10 {
                        scenario
                            .press_key("j")
                            .wait_for_text("Enter: load more", 5000)
                            .press_key("Enter")
                            .wait_for_text("archive-10", 5000)
                            .capture_labeled("loaded", "Next page restores a loadable archive")
                    } else {
                        scenario
                    }
                },
                move |frame, report| {
                    Box::pin(async move {
                        // Assert
                        let skipped = common::frame_from_capture(&report.captures[0]);
                        let full = Region::full(skipped.cols(), skipped.rows());
                        assertion::assert_text_in_region(
                            &skipped,
                            &format!("ARCHIVE —— {archive_count}"),
                            &full,
                        );
                        assertion::assert_not_visible(&skipped, "archive-00");
                        assertion::assert_not_visible(&skipped, "No sessions.");
                        if archive_count > 10 {
                            let full = Region::full(frame.cols(), frame.rows());
                            assertion::assert_text_in_region(frame, "ARCHIVE —— 11", &full);
                            assertion::assert_text_in_region(frame, "archive-10", &full);
                            assertion::assert_not_visible(frame, "Load more...");
                        } else {
                            assertion::assert_not_visible(&skipped, "Load more...");
                        }
                    })
                },
            )
            .await?;
    }

    Ok(())
}
