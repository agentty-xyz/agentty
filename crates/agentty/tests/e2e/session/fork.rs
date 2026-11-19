//! Session forking from review-ready sessions.

use testty::assertion;
use testty::region::Region;

use super::fixture::{E2eResult, seed_linked_review_worktree_with_diff, seed_review_ready_session};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Seeds a committed source change plus an uncommitted edit so fork counts
/// must reflect the frozen branch tip rather than stale source statistics.
async fn seed_dirty_fork_source_session(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env).await?;
    seed_linked_review_worktree_with_diff(env)?;
    std::fs::write(
        env.agentty_root.join("wt/review-s/src/main.rs"),
        "uncommitted source edit\n",
    )?;

    Ok(())
}

/// Verify that review-ready sessions can confirm session forking from
/// session view.
#[tokio::test]
async fn session_fork_confirmation_creates_session_from_review_session() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_fork_confirmation")
        .with_git()
        .with_terminal_size(120, 24)
        .setup(|env| Box::pin(async move { seed_dirty_fork_source_session(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("?")
                    .wait_for_text("F: Fork session", 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("session_view", "Fork shortcut listed in session help")
                    .press_key("Escape")
                    .step(common::wait_for_frame_text(
                        &["Review-ready session shortcuts"],
                        &["Keybindings"],
                    ))
                    .write_text("F")
                    .wait_for_text("Confirm Fork", 3000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("fork_confirmation", "Fork confirmation popup")
                    .write_text("y")
                    .wait_for_text("Review-ready session shortcuts", 5000)
                    .wait_for_text("+3 -0", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("forked_session_view", "Forked session opened")
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("Tab")
                    .wait_for_text("j/k: scroll", 5000)
                    .capture_labeled(
                        "forked_session_chat_focus",
                        "Fork keeps chat navigation available",
                    )
                    .press_key("q")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("session_list_after_fork", "Source and fork listed")
            },
            |frame, report| {
                Box::pin(async move {
                    let session_view_frame = common::frame_from_capture(&report.captures[0]);
                    let view_full =
                        Region::full(session_view_frame.cols(), session_view_frame.rows());
                    assertion::assert_text_in_region(
                        &session_view_frame,
                        "F: Fork session",
                        &view_full,
                    );

                    let confirmation_frame = common::frame_from_capture(&report.captures[1]);
                    let confirmation_full =
                        Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                    assertion::assert_text_in_region(
                        &confirmation_frame,
                        "Confirm Fork",
                        &confirmation_full,
                    );
                    assertion::assert_text_in_region(
                        &confirmation_frame,
                        "Fork this session",
                        &confirmation_full,
                    );

                    let forked_view_frame = common::frame_from_capture(&report.captures[2]);
                    let forked_view_full =
                        Region::full(forked_view_frame.cols(), forked_view_frame.rows());
                    assertion::assert_text_in_region(
                        &forked_view_frame,
                        "Review-ready session shortcuts",
                        &forked_view_full,
                    );
                    assertion::assert_not_visible(&forked_view_frame, "Confirm Fork");
                    assertion::assert_text_in_region(
                        &forked_view_frame,
                        "+3 -0",
                        &forked_view_full,
                    );

                    let forked_chat_frame = common::frame_from_capture(&report.captures[3]);
                    let forked_chat_full =
                        Region::full(forked_chat_frame.cols(), forked_chat_frame.rows());
                    assertion::assert_text_in_region(
                        &forked_chat_frame,
                        "Tab: focus | j/k: scroll",
                        &forked_chat_full,
                    );

                    let full = Region::full(frame.cols(), frame.rows());
                    let session_list_text = frame.text_in_region(&full);
                    let session_title_count = session_list_text
                        .matches("Review-ready session shortcuts")
                        .count();
                    assert!(
                        session_title_count >= 2,
                        "expected source and fork rows in session list, got \
                         {session_title_count}:\n{session_list_text}"
                    );
                })
            },
        )
        .await?;

    Ok(())
}
