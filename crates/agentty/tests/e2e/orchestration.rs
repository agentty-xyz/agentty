//! Orchestration campaign and managed-worker E2E tests.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use agentty::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
};
use agentty::domain::session_message::SessionMessageKind;
use agentty::test_support;
use testty::assertion;
use testty::frame::TerminalFrame;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

type E2eResult = Result<(), Box<dyn std::error::Error>>;

const CONTROLLER_ID: &str = "controller-0001";
const WORKER_ID: &str = "worker-a-0001";
const CONTROLLER_REVISION_PROMPT: &str = "Revise the plan without changing branches";
const CONTROLLER_REVISION_RESPONSE: &str = "Controller revision recorded.";

/// Seeds one parked campaign plus a linked managed worker for UI proofs.
fn seed_orchestration_campaign(env: &BuilderEnv) -> E2eResult {
    seed_orchestration_campaign_rows(env)?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("controll"))?;
    std::fs::create_dir_all(env.agentty_root.join("wt").join("worker-a"))?;

    Ok(())
}

/// Seeds a controller and managed worker with real diffs so startup and turn
/// completion exercise role-scoped automatic review.
fn seed_orchestrator_auto_review_scope(env: &BuilderEnv) -> E2eResult {
    seed_orchestration_campaign_rows(env)?;
    seed_orchestration_review_worktrees(env)?;
    install_auto_review_claude_stub(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        for session_id in [CONTROLLER_ID, WORKER_ID] {
            database
                .sessions()
                .update_session_model(session_id, "claude-haiku-4-5-20251001")
                .await?;
            database
                .sessions()
                .update_session_diff_stats(1, 0, true, session_id, "XS")
                .await?;
        }
        database
            .sessions()
            .update_session_status_with_timing_at(WORKER_ID, "Review", 0)
            .await?;

        sqlx::query(
            "UPDATE session_orchestration SET status = 'Running' WHERE controller_session_id = ?",
        )
        .bind(CONTROLLER_ID)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "UPDATE session_orchestration_task SET status = CASE task_key WHEN 'protocol' THEN \
             'Reviewing' ELSE 'Failed' END",
        )
        .execute(database.pool())
        .await?;

        let project_id =
            sqlx::query_scalar::<_, i64>("SELECT project_id FROM session WHERE id = ?")
                .bind(CONTROLLER_ID)
                .fetch_one(database.pool())
                .await?;
        for (setting_name, setting_value) in [
            ("DefaultReviewAgent", "claude"),
            ("DefaultReviewModel", "claude-haiku-4-5-20251001"),
        ] {
            sqlx::query(
                "INSERT INTO project_setting (project_id, name, value) VALUES (?, ?, ?) ON \
                 CONFLICT(project_id, name) DO UPDATE SET value = excluded.value",
            )
            .bind(project_id)
            .bind(setting_name)
            .bind(setting_value)
            .execute(database.pool())
            .await?;
        }

        database
            .sessions()
            .update_session_title(CONTROLLER_ID, "Managed feature delivery")
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

/// Seeds the persisted rows for one parked campaign and linked managed worker.
fn seed_orchestration_campaign_rows(env: &BuilderEnv) -> E2eResult {
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
        // Keep the controller as the deterministic initial raw selection so
        // one `j` press always reaches its grouped worker row.
        database
            .sessions()
            .update_session_title(CONTROLLER_ID, "Managed feature delivery")
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    });
    seed_result?;

    Ok(())
}

/// Creates linked controller and worker worktrees with one tracked change in
/// each branch.
fn seed_orchestration_review_worktrees(env: &BuilderEnv) -> E2eResult {
    std::fs::write(env.workdir.join("review-scope.txt"), "base\n")?;
    run_git_command(&env.workdir, &["add", "review-scope.txt"])?;
    run_git_command(&env.workdir, &["commit", "-m", "review scope base"])?;

    let worktree_root = env.agentty_root.join("wt");
    std::fs::create_dir_all(&worktree_root)?;
    for (session_id, branch_name, content) in [
        (CONTROLLER_ID, "wt/controll", "controller change\n"),
        (WORKER_ID, "wt/worker-a", "worker change\n"),
    ] {
        let session_folder = test_support::session_folder(&worktree_root, session_id);
        let session_folder_text = session_folder.to_string_lossy().into_owned();
        run_git_command(
            &env.workdir,
            &[
                "worktree",
                "add",
                "-b",
                branch_name,
                session_folder_text.as_str(),
            ],
        )?;
        std::fs::write(session_folder.join("review-scope.txt"), content)?;
    }

    Ok(())
}

/// Installs a prompt-aware Claude stub that completes controller chat quickly
/// while leaving focused review visibly in progress.
fn install_auto_review_claude_stub(env: &BuilderEnv) -> E2eResult {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r###"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
input=$(cat)
case "$input" in
  *"Review the Git diff for display in a terminal UI."*)
    sleep 30
    result='{{\"answer\":\"## Review\\n\\n### Project Impact\\n\\n- Managed worker review completed.\\n\\n### Suggestions\\n\\n- None\",\"questions\":[],\"summary\":null}}'
    ;;
  *)
    result='{{\"answer\":\"{CONTROLLER_REVISION_RESPONSE}\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[],\"summary\":null,\"verification_verdicts\":[]}}'
    ;;
esac
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '{{"type":"result","subtype":"success","result":"%s","usage":{{"input_tokens":5,"output_tokens":9}}}}\n' "$result"
"###,
    );
    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;

    Ok(())
}

/// Runs one Git setup command inside the isolated E2E repository.
fn run_git_command(working_directory: &Path, args: &[&str]) -> E2eResult {
    let output = Command::new("git")
        .args(args)
        .current_dir(working_directory)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr),
        ))
        .into());
    }

    Ok(())
}

/// Seeds a completed read-only research task whose temporary worktree had
/// edits so the campaign board proves both report capture and discard status.
fn seed_reported_research_task(env: &BuilderEnv) -> E2eResult {
    seed_orchestration_campaign(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        sqlx::query(
            "UPDATE session_orchestration_task SET kind = 'Research', title = 'Architecture \
             review', prompt = 'Inspect architecture', touched_areas = '[]', status = 'Reported', \
             research_report = 'Architecture boundaries are documented', verification_verdict = \
             'Pass' WHERE task_key = 'protocol'",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "UPDATE session SET role = 'OrchestrationResearcher', has_diff = 1, status = \
             'Canceled', title = 'Inspect architecture boundaries', archived_diff = 'diff --git \
             a/policy.txt b/policy.txt\n+Unexpected research write\n' WHERE id = ?",
        )
        .bind(WORKER_ID)
        .execute(database.pool())
        .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Seeds one managed worker executing its first accepted review-remediation
/// continuation so the campaign board exposes stable remediation progress.
fn seed_orchestration_review_remediation(env: &BuilderEnv) -> E2eResult {
    seed_orchestration_campaign(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        let task_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM session_orchestration_task WHERE task_key = 'protocol'",
        )
        .fetch_one(database.pool())
        .await?;
        sqlx::query(
            "UPDATE session_orchestration_task SET status = 'ReviewApplying', review_iteration = \
             1, continuation_generation = 1, continuation_prompt = 'Verify then apply sensible \
             review suggestions' WHERE id = ?",
        )
        .bind(task_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "UPDATE session SET status = 'InProgress', has_diff = 1, focused_review_status = \
             NULL, focused_review_diff_hash = NULL, focused_review_text = NULL WHERE id = ?",
        )
        .bind(WORKER_ID)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO session_operation (id, session_id, kind, status, queued_at, finished_at) \
             VALUES (?, ?, 'reply', 'done', 1, 1)",
        )
        .bind(format!("orchestration-continuation-{task_id}-1"))
        .bind(WORKER_ID)
        .execute(database.pool())
        .await?;
        sqlx::query("UPDATE session_orchestration SET status = 'Running'")
            .execute(database.pool())
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Seeds enough controller transcript output to exercise line-by-line scroll
/// bounds in the compact chat pane below the campaign board.
fn seed_scrollable_orchestration_campaign(env: &BuilderEnv) -> E2eResult {
    seed_orchestration_campaign(env)?;

    let output = (0..60)
        .map(|line_index| format!("Transcript line {line_index:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let output = format!("```text\n{output}\n```");
    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(CONTROLLER_ID, SessionMessageKind::AssistantAnswer, &output)
            .await
    })?;

    Ok(())
}

/// Seeds one parked campaign whose managed worker is ready for review.
fn seed_review_ready_managed_worker(env: &BuilderEnv) -> E2eResult {
    seed_orchestration_campaign(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_status_with_timing_at(WORKER_ID, "Review", 0)
            .await?;
        // The worker status update touches its row, so restore the same stable
        // initial selection ordering used by the base campaign seed.
        database
            .sessions()
            .update_session_title(CONTROLLER_ID, "Managed feature delivery")
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

/// Seeds a review-request campaign with one worker still awaiting its forge
/// merge and one already integrated sibling.
fn seed_review_request_campaign_awaiting_merge(env: &BuilderEnv) -> E2eResult {
    seed_orchestration_campaign(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        sqlx::query!(
            "UPDATE session_orchestration SET status = 'Integrating', integration_approach = \
             'ReviewRequest'",
        )
        .execute(database.pool())
        .await?;
        sqlx::query!(
            "UPDATE session_orchestration_task SET status = CASE task_key WHEN 'protocol' THEN \
             'ReviewRequested' ELSE 'Integrated' END",
        )
        .execute(database.pool())
        .await?;
        sqlx::query!(
            "UPDATE session SET status = 'Review' WHERE id = ?",
            WORKER_ID
        )
        .execute(database.pool())
        .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

/// Seeds a review-request campaign whose forge review was closed without a
/// merge so reconciliation must surface an integration failure.
fn seed_review_request_campaign_with_closed_worker(env: &BuilderEnv) -> E2eResult {
    seed_review_request_campaign_awaiting_merge(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        sqlx::query!(
            "UPDATE session SET status = 'Canceled' WHERE id = ?",
            WORKER_ID
        )
        .execute(database.pool())
        .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

/// Returns the numbered transcript rows visible in one captured frame.
fn visible_transcript_line_indexes(frame: &TerminalFrame) -> Vec<u16> {
    let full = Region::full(frame.cols(), frame.rows());

    frame
        .text_in_region(&full)
        .lines()
        .filter_map(|line| {
            let suffix = line.split_once("Transcript line ")?.1;

            suffix.get(..2)?.parse().ok()
        })
        .collect()
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
fn test_orchestrator_auto_review_scope() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("orchestrator_auto_review_scope")
        .with_git()
        .setup(seed_orchestrator_auto_review_scope)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("Campaign: Managed feature delivery", 5000)
                    .press_key("Enter")
                    .wait_for_text("Tab: focus | Enter: send", 5000)
                    .write_text(CONTROLLER_REVISION_PROMPT)
                    .press_key("Enter")
                    .wait_for_text(CONTROLLER_REVISION_RESPONSE, 30000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "controller_review_ready",
                        "Controller remains review-ready after its turn",
                    )
                    .compose(&common::return_to_session_list())
                    .press_key("j")
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("Managed by controller-0001", 5000)
                    .wait_for_text("Reviewing changes with claude-haiku-4-5-20251001", 5000)
                    .capture_labeled(
                        "worker_auto_review",
                        "Managed worker starts focused review automatically",
                    )
            },
            |frame, report| {
                let controller_frame = common::frame_from_capture(&report.captures[0]);
                let controller_full =
                    Region::full(controller_frame.cols(), controller_frame.rows());
                assertion::assert_text_in_region(
                    &controller_frame,
                    CONTROLLER_REVISION_RESPONSE,
                    &controller_full,
                );
                assertion::assert_text_in_region(
                    &controller_frame,
                    "Phase: Running",
                    &controller_full,
                );
                assertion::assert_not_visible(&controller_frame, "Reviewing changes with");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Managed by controller-0001", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Reviewing changes with claude-haiku-4-5-20251001",
                    &full,
                );
            },
        )
}

#[test]
fn test_review_request_campaign_waits_for_worker_merge() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_campaign_waits_for_worker_merge")
        .with_git()
        .setup(seed_review_request_campaign_awaiting_merge)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    // Campaign reconciliation competes with up to four E2E
                    // processes on CI, so allow one longer render deadline.
                    .wait_for_text("Phase: Integrating", 10_000)
                    .capture_labeled(
                        "controller_waiting",
                        "Controller remains active while review request is open",
                    )
                    .compose(&common::return_to_session_list())
                    .press_key("j")
                    .wait_for_stable_frame(300, 3000)
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("Managed by controller-0001", 5000)
                    .press_key("?")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "worker_attached",
                        "Review-request worker remains attached to controller",
                    )
            },
            |frame, report| {
                let controller_frame = common::frame_from_capture(&report.captures[0]);
                let controller_full =
                    Region::full(controller_frame.cols(), controller_frame.rows());
                assertion::assert_text_in_region(
                    &controller_frame,
                    "Phase: Integrating",
                    &controller_full,
                );
                assertion::assert_text_in_region(
                    &controller_frame,
                    "Protocol contract [protocol]: review requested",
                    &controller_full,
                );
                let controller_text = controller_frame.text_in_region(&controller_full);
                assert!(!controller_text.contains("Campaign complete"));

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Managed by controller-0001", &full);
                assertion::assert_text_in_region(frame, "Detach managed", &full);
            },
        )
}

#[test]
fn test_review_request_campaign_reports_closed_worker() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_campaign_reports_closed_worker")
        .with_git()
        .setup(seed_review_request_campaign_with_closed_worker)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("integration failed", 5000)
                    .capture_labeled(
                        "closed_worker_failure",
                        "Closed review request remains visible as integration failure",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Phase: Integrating", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Protocol contract [protocol]: integration failed",
                    &full,
                );
                let text = frame.text_in_region(&full);
                assert!(!text.contains("Campaign complete"));
            },
        )
}

#[test]
fn test_orchestration_review_remediation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("orchestration_review_remediation")
        .with_git()
        .setup(seed_orchestration_review_remediation)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("remediation 1/3", 5000)
                    .capture_labeled(
                        "review_remediation",
                        "Managed worker focused-review remediation progress",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Protocol contract [protocol]: applying review; remediation 1/3",
                    &full,
                );
            },
        )
}

#[test]
fn test_orchestration_research_report() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("orchestration_research_report")
        .with_git()
        .setup(seed_reported_research_task)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("[Research] Architecture review", 5000)
                    .capture_labeled(
                        "research_report",
                        "Read-only research report on campaign board",
                    )
                    .press_key("q")
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Managed by controller-0001", 5000)
                    .wait_for_text("Inspect architecture boundaries", 5000)
                    .press_key("d")
                    .wait_for_text("Unexpected research write", 5000)
                    .capture_labeled(
                        "research_archived_diff",
                        "Archived research diff after temporary cleanup",
                    )
            },
            |frame, report| {
                let report_frame = common::frame_from_capture(&report.captures[0]);
                let report_full = Region::full(report_frame.cols(), report_frame.rows());
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    &report_frame,
                    "[Research] Architecture review [protocol]: reported",
                    &report_full,
                );
                assertion::assert_text_in_region(&report_frame, "report captured;", &report_full);
                assertion::assert_text_in_region(
                    &report_frame,
                    "temporary edits discarded; verified",
                    &report_full,
                );
                assertion::assert_text_in_region(frame, "Unexpected research write", &full);
                assertion::assert_text_in_region(frame, "Inspect architecture boundaries", &full);
                assertion::assert_text_in_region(frame, "policy.txt", &full);
                assertion::assert_text_in_region(frame, "q/Esc: back", &full);
            },
        )
}

#[test]
fn test_orchestration_campaign_smooth_scroll() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("orchestration_campaign_smooth_scroll")
        .with_git()
        .with_terminal_size(80, 24)
        .setup(seed_scrollable_orchestration_campaign)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("Transcript line 59", 5000)
                    .capture_labeled("campaign_bottom", "Controller transcript at bottom")
                    .press_key("k")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("campaign_one_line_up", "Controller transcript one line up")
            },
            |_frame, report| {
                assert_eq!(report.captures.len(), 2);

                let bottom_frame = common::frame_from_capture(&report.captures[0]);
                let one_line_up_frame = common::frame_from_capture(&report.captures[1]);
                let bottom_lines = visible_transcript_line_indexes(&bottom_frame);
                let one_line_up_lines = visible_transcript_line_indexes(&one_line_up_frame);
                assert!(!bottom_lines.is_empty(), "expected bottom transcript rows");
                assert!(
                    !one_line_up_lines.is_empty(),
                    "expected scrolled transcript rows"
                );
                assert_eq!(
                    one_line_up_lines
                        .first()
                        .copied()
                        .map(|line_index| line_index + 1),
                    bottom_lines.first().copied()
                );
            },
        )
}

#[test]
fn test_managed_worker_restricted_view() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("managed_worker_restricted_view")
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
                    .capture_labeled("managed_worker", "Managed worker restricted actions")
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
                assertion::assert_text_in_region(frame, "actions restricted", &full);
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
fn test_managed_worker_review_open_action() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("managed_worker_review_open_action")
        .with_git()
        .setup(seed_review_ready_managed_worker)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Managed by controller-0001", 5000)
                    .compose(&common::open_help_overlay())
                    .capture_labeled(
                        "managed_worker_review_help",
                        "Review-ready managed worker exposes worktree open",
                    )
                    .press_key("q")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("o")
                    .wait_for_text("Open Managed Worktree", 5000)
                    .capture_labeled(
                        "managed_worker_open_warning",
                        "Managed worktree write-access warning",
                    )
            },
            |frame, report| {
                let help_frame = common::frame_from_capture(&report.captures[0]);
                let help_full = Region::full(help_frame.cols(), help_frame.rows());
                assertion::assert_text_in_region(&help_frame, "o: Open worktree", &help_full);
                let help_text = help_frame.text_in_region(&help_full);
                assert!(!help_text.contains("Enter: Reply"));
                assert!(!help_text.contains("m: Add to merge queue"));

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Managed by controller-0001", &full);
                assertion::assert_text_in_region(frame, "Open Managed Worktree", &full);
                assertion::assert_text_in_region(frame, "This opens a writable", &full);
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
