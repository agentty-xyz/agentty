//! Orchestration campaign and managed-worker E2E tests.

use agentty::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
};
use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

type E2eResult = Result<(), Box<dyn std::error::Error>>;

const CONTROLLER_ID: &str = "controller-0001";
const WORKER_ID: &str = "worker-a-0001";

/// Seeds one parked campaign plus a linked managed worker for UI proofs.
fn seed_orchestration_campaign(env: &BuilderEnv) -> E2eResult {
    common::seed_session(
        env,
        SessionSeed::regular(CONTROLLER_ID, "gpt-5.6-sol", "main", "Review")
            .with_title("Managed feature delivery"),
    )?;
    common::seed_session(
        env,
        SessionSeed::regular(WORKER_ID, "gpt-5.6-sol", "main", "InProgress")
            .with_title("Implement protocol contract"),
    )?;

    let runtime = common::seed_runtime()?;
    let seed_result = runtime.block_on(async {
        let database = common::open_database(env)
            .await
            .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
        sqlx::query("UPDATE session SET role = 'Orchestrator' WHERE id = ?")
            .bind(CONTROLLER_ID)
            .execute(database.pool())
            .await?;
        let orchestration_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO session_orchestration (controller_session_id, goal_statement, status, \
             max_parallelism, created_at, updated_at) VALUES (?, 'Deliver the campaign safely', \
             'AwaitingApproval', 3, 1, 1) RETURNING id",
        )
        .bind(CONTROLLER_ID)
        .fetch_one(database.pool())
        .await?;
        let protocol_task_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO session_orchestration_task (session_orchestration_id, task_key, title, \
             prompt, touched_areas, acceptance_criteria, merge_position, status, \
             child_session_id, created_at, updated_at) VALUES (?, 'protocol', 'Protocol \
             contract', 'Implement the protocol contract', '[\"crates/ag-protocol/\"]', \
             '[\"Protocol tests pass\"]', 0, 'Proposed', ?, 1, 1) RETURNING id",
        )
        .bind(orchestration_id)
        .bind(WORKER_ID)
        .fetch_one(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO session_orchestration_task (session_orchestration_id, task_key, title, \
             prompt, touched_areas, acceptance_criteria, merge_position, status, created_at, \
             updated_at) VALUES (?, 'ui', 'Campaign board', 'Implement the campaign board', \
             '[\"crates/agentty/src/ui/\"]', '[\"Board is visible\"]', 1, 'Proposed', 1, 1)",
        )
        .bind(orchestration_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "UPDATE session SET role = 'OrchestrationWorker', orchestration_task_id = ? WHERE id \
             = ?",
        )
        .bind(protocol_task_id)
        .bind(WORKER_ID)
        .execute(database.pool())
        .await?;
        database
            .reviews()
            .update_session_review_request(
                WORKER_ID,
                Some(ReviewRequest {
                    last_refreshed_at: 1,
                    summary: ReviewRequestSummary {
                        display_id: "#7".to_string(),
                        forge_kind: ForgeKind::GitHub,
                        source_branch: "wt/worker-a".to_string(),
                        state: ReviewRequestState::Open,
                        status_summary: None,
                        target_branch: "main".to_string(),
                        title: "Managed worker review".to_string(),
                        web_url: "https://github.com/example/project/pull/7".to_string(),
                    },
                }),
            )
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    });
    seed_result?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("controll"))?;
    std::fs::create_dir_all(env.agentty_root.join("wt").join("worker-a"))?;

    Ok(())
}

/// Seeds one completed managed worker whose worktree-independent review diff
/// has already been archived by the merge workflow.
fn seed_archived_managed_worker(env: &BuilderEnv) -> E2eResult {
    seed_orchestration_campaign(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_archived_diff(
                WORKER_ID,
                Some(
                    "diff --git a/worker.txt b/worker.txt\nnew file mode 100644\n+Archived worker \
                     result\n"
                        .to_string(),
                ),
            )
            .await?;
        sqlx::query("UPDATE session SET status = 'Done' WHERE id IN (?, ?)")
            .bind(WORKER_ID)
            .bind(CONTROLLER_ID)
            .execute(database.pool())
            .await?;
        sqlx::query("UPDATE session_orchestration SET status = 'Done'")
            .execute(database.pool())
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

#[test]
fn test_orchestration_campaign_board() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("orchestration_campaign_board")
        .with_git()
        .setup(seed_orchestration_campaign)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .capture_labeled("campaign_board", "Parked orchestration campaign board")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Campaign: Managed feature delivery",
                    &full,
                );
                assertion::assert_text_in_region(frame, "Phase: AwaitingApproval", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Parallel workers: 3 (global setting)",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "Protocol contract [protocol]: awaiting approval",
                    &full,
                );
                assertion::assert_text_in_region(frame, "a approve  Enter discuss/revise", &full);
            },
        )
}

#[test]
fn test_managed_worker_read_only_view() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("managed_worker_read_only_view")
        .with_git()
        .setup(seed_orchestration_campaign)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Working...", 5000)
                    .press_key("ctrl+c")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "managed_worker_running",
                        "Managed worker remains active after Ctrl+c",
                    )
                    .press_key("c")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("?")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("managed_worker", "Managed worker read-only actions")
            },
            |frame, report| {
                let running_frame = common::frame_from_capture(&report.captures[0]);
                let running_full = Region::full(running_frame.cols(), running_frame.rows());
                assertion::assert_text_in_region(&running_frame, "Working...", &running_full);
                assertion::assert_text_in_region(
                    &running_frame,
                    "Managed by controller-0001",
                    &running_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Managed by controller-0001", &full);
                assertion::assert_text_in_region(frame, "read-only", &full);
                assertion::assert_text_in_region(frame, "d: Show diff", &full);
                assertion::assert_text_in_region(frame, "Detach managed", &full);
                let text = frame.text_in_region(&full);
                assert!(!text.contains("Enter: Reply"));
                assert!(!text.contains("p: Create or refresh"));
                assert!(!text.contains("Review comments"));
            },
        )
}

#[test]
fn managed_worker_archived_diff() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("managed_worker_archived_diff")
        .with_git()
        .setup(seed_archived_managed_worker)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Managed by controller-0001", 5000)
                    .press_key("d")
                    .wait_for_text("Archived worker result", 5000)
                    .capture_labeled(
                        "archived_worker_diff",
                        "Archived managed worker diff after cleanup",
                    )
                    .press_key("q")
                    .press_key("q")
                    .press_key("k")
                    .press_key("Enter")
                    .wait_for_text("Campaign complete", 5000)
                    .capture_labeled("completed_campaign", "Completed campaign archive")
            },
            |frame, report| {
                let diff_frame = common::frame_from_capture(&report.captures[0]);
                let diff_full = Region::full(diff_frame.cols(), diff_frame.rows());
                assertion::assert_text_in_region(&diff_frame, "Archived worker result", &diff_full);
                assertion::assert_text_in_region(&diff_frame, "worker.txt", &diff_full);
                assertion::assert_text_in_region(&diff_frame, "q/Esc: back", &diff_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Phase: Done", &full);
                assertion::assert_text_in_region(frame, "Campaign complete", &full);
            },
        )
}
