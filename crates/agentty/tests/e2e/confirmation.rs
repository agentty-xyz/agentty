//! Confirmation dialog E2E tests: quit-confirm-yes exits, quit-confirm
//! dismiss via `n` and `Esc` returns to the app.

use std::time::Duration;

use ag_session::test_support as model_fixture;
use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Seeds one unstarted draft session for list-mode cancel confirmation.
async fn seed_cancelable_draft_session(env: &BuilderEnv) -> E2eResult {
    common::seed_session(
        env,
        SessionSeed::draft(
            "draft-cancel-0001",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Draft",
        )
        .with_title("Cancel staged draft from list"),
    )
    .await
}

/// Seeds one running session for list-mode cancel confirmation.
async fn seed_cancelable_running_session(env: &BuilderEnv) -> E2eResult {
    let session_id = "running-cancel-0001";

    common::seed_session(
        env,
        SessionSeed::regular(
            session_id,
            model_fixture::CODEX_MODEL_ID,
            "main",
            "InProgress",
        )
        .with_title("Cancel running session"),
    )
    .await?;

    let worktree_name = &session_id[..8];
    // Match `session_folder()` so the seeded in-progress row remains visible
    // in the list and can participate in cancel flow assertions.
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    Ok(())
}

/// Seeds one session waiting on clarification questions for list-mode cancel
/// confirmation.
async fn seed_cancelable_question_session(env: &BuilderEnv) -> E2eResult {
    let session_id = "question-cancel-0001";

    common::seed_session(
        env,
        SessionSeed::regular(
            session_id,
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Question",
        )
        .with_title("Cancel question session"),
    )
    .await?;
    common::open_database(env)
        .await?
        .sessions()
        .update_session_questions(session_id, r#"[{"text":"Which file?","options":[]}]"#)
        .await?;

    let worktree_name = &session_id[..8];
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    Ok(())
}

/// Verify that confirming quit with `y` causes the process to exit with
/// code 0.
///
/// Opens the quit dialog, presses `y`, and asserts that the PTY child
/// process terminates successfully. The exit wait is intentionally longer
/// than a regular PTY assertion because coverage builds and CI hosts can
/// take extra time to unwind terminal cleanup on shutdown.
#[tokio::test]
async fn quit_confirm_yes_exits() {
    // Arrange
    let _test_guard = common::acquire_e2e_test_lock().await;
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");
    common::open_database(&env).await.expect("fixture database");
    let mut session = env.builder().spawn().expect("failed to spawn session");

    let scenario = Scenario::new("quit_yes")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::open_quit_dialog())
        .press_key("y");

    // Act
    scenario
        .execute_in_pty(&mut session)
        .expect("scenario execution failed");

    // Assert — process should exit with code 0.
    let exited_successfully = session
        .wait_for_exit(Duration::from_secs(15))
        .expect("process did not exit within timeout");

    assert!(exited_successfully, "Process should exit with code 0");
}

/// Verify that dismissing the quit dialog with `n` and `Esc` returns to
/// the normal app view.
///
/// Opens the quit dialog twice: first dismisses with `n`, then with `Esc`.
/// After each dismissal, asserts that the app is back on the Projects tab.
#[tokio::test]
async fn quit_confirm_dismiss_returns() {
    // Arrange, Act, Assert
    FeatureTest::new("quit_dismiss")
        .with_git()
        .setup(|env| Box::pin(common::seed_projects_startup_tab(env)))
        .zola(
            "Quit dismiss",
            "Dismiss the quit dialog to stay in the session.",
            140,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .compose(&common::open_quit_dialog())
                    .viewing_pause_ms(2500)
                    .capture_labeled("dialog_n", "Quit dialog before n")
                    .press_key("n")
                    .wait_for_stable_frame(300, 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("after_n", "App restored after n")
                    .compose(&common::open_quit_dialog())
                    .viewing_pause_ms(2500)
                    .capture_labeled("dialog_esc", "Quit dialog before Esc")
                    .press_key("Escape")
                    .wait_for_stable_frame(300, 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("after_esc", "App restored after Esc")
            },
            |frame, report| {
                Box::pin(async move {
                    let dialog_n_frame = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(dialog_n_frame.cols(), dialog_n_frame.rows());
                    assertion::assert_text_in_region(&dialog_n_frame, "Confirm Quit", &full);

                    let dialog_esc_frame = common::frame_from_capture(&report.captures[2]);
                    let full_esc = Region::full(dialog_esc_frame.cols(), dialog_esc_frame.rows());
                    assertion::assert_text_in_region(&dialog_esc_frame, "Confirm Quit", &full_esc);

                    let after_n_frame = common::frame_from_capture(&report.captures[1]);
                    let restored_full = Region::full(after_n_frame.cols(), after_n_frame.rows());
                    assertion::assert_text_in_region(
                        &after_n_frame,
                        "test-project",
                        &restored_full,
                    );

                    let after_n_text = after_n_frame.text_in_region(&restored_full);
                    assert!(
                        !after_n_text.contains("Confirm Quit"),
                        "Quit dialog should be dismissed after 'n'"
                    );

                    let final_full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "test-project", &final_full);

                    let final_text = frame.text_in_region(&final_full);
                    assert!(
                        !final_text.contains("Confirm Quit"),
                        "Quit dialog should be dismissed after Esc"
                    );
                })
            },
        )
        .await
        .expect("feature test failed");
}

/// Verify that an unstarted draft session can be canceled directly from the
/// session list.
#[tokio::test]
async fn draft_session_cancel_confirmation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("draft_session_cancel")
        .with_git()
        .setup(|env| Box::pin(async move { seed_cancelable_draft_session(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Cancel staged draft from list", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "draft_row",
                        "Draft session visible in the session list with cancel available",
                    )
                    .press_key("c")
                    .wait_for_text("Confirm Cancel", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "confirm_cancel",
                        "Cancel confirmation for the selected draft session",
                    )
                    .press_key("y")
                    .wait_for_text("Canceled", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "canceled_draft",
                        "Canceled draft session moved into the archive group",
                    )
            },
            |frame, report| {
                Box::pin(async move {
                    let list_frame = common::frame_from_capture(&report.captures[0]);
                    let list_full = Region::full(list_frame.cols(), list_frame.rows());
                    assertion::assert_text_in_region(
                        &list_frame,
                        "Cancel staged draft from list",
                        &list_full,
                    );
                    assertion::assert_text_in_region(&list_frame, "Draft", &list_full);

                    let confirmation_frame = common::frame_from_capture(&report.captures[1]);
                    let confirmation_full =
                        Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                    assertion::assert_text_in_region(
                        &confirmation_frame,
                        "Confirm Cancel",
                        &confirmation_full,
                    );

                    let final_full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "ARCHIVE —— 1", &final_full);
                    assertion::assert_text_in_region(frame, "Canceled", &final_full);
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that a running session can be canceled directly from the session
/// list.
#[tokio::test]
async fn running_session_cancel_confirmation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("running_session_cancel")
        .with_git()
        .setup(|env| Box::pin(async move { seed_cancelable_running_session(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Cancel running session", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "running_row",
                        "Running session visible in the session list with cancel available",
                    )
                    .press_key("c")
                    .wait_for_text("Confirm Cancel", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "confirm_cancel",
                        "Cancel confirmation for the selected running session",
                    )
                    .press_key("y")
                    .wait_for_text("Canceled", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "canceled_running",
                        "Canceled running session moved into the archive group",
                    )
            },
            |frame, report| {
                Box::pin(async move {
                    let list_frame = common::frame_from_capture(&report.captures[0]);
                    let list_full = Region::full(list_frame.cols(), list_frame.rows());
                    assertion::assert_text_in_region(
                        &list_frame,
                        "Cancel running session",
                        &list_full,
                    );

                    let confirmation_frame = common::frame_from_capture(&report.captures[1]);
                    let confirmation_full =
                        Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                    assertion::assert_text_in_region(
                        &confirmation_frame,
                        "Confirm Cancel",
                        &confirmation_full,
                    );

                    let final_full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "ARCHIVE", &final_full);
                    assertion::assert_text_in_region(frame, "Canceled", &final_full);
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that a session waiting on clarification questions can be canceled
/// directly from the session list.
#[tokio::test]
async fn question_session_cancel_confirmation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("question_session_cancel")
        .with_git()
        .setup(|env| Box::pin(async move { seed_cancelable_question_session(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Cancel question session", 5000)
                    .capture_labeled(
                        "question_row",
                        "Question session visible in the session list",
                    )
                    .press_key("c")
                    .wait_for_text("Confirm Cancel", 3000)
                    .capture_labeled(
                        "confirm_cancel",
                        "Cancel confirmation for the selected question session",
                    )
                    .press_key("y")
                    .wait_for_text("Canceled", 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    let list_frame = common::frame_from_capture(&report.captures[0]);
                    let list_full = Region::full(list_frame.cols(), list_frame.rows());
                    assertion::assert_text_in_region(&list_frame, "Question", &list_full);

                    let confirmation_frame = common::frame_from_capture(&report.captures[1]);
                    let confirmation_full =
                        Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                    assertion::assert_text_in_region(
                        &confirmation_frame,
                        "Confirm Cancel",
                        &confirmation_full,
                    );

                    let final_full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "ARCHIVE", &final_full);
                    assertion::assert_text_in_region(frame, "Canceled", &final_full);
                })
            },
        )
        .await?;

    Ok(())
}
