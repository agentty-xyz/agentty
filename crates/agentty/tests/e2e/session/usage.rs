//! The per-session usage panel, opened with `u` and `/usage`.

use std::time::Duration;

use ag_contracts::{CostBasis, CostEstimate, ModelUsage, TurnUsage, UsagePurpose};
use testty::assertion;
use testty::proof::report::ProofReport;
use testty::region::Region;
use testty::scenario::Scenario;

use super::fixture::{E2eResult, seed_review_ready_session};
use super::harness::{
    HARNESS_ANSWER, NO_LOCAL_SERVER, scripted_provider, switch_to_harness_muse_model,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Row that closes the usage table.
const SESSION_TOTAL: &str = "Session total";

/// Bottom-border hint shown when the panel overflows the terminal.
const SCROLL_HINT: &str = "j/k: scroll";

/// Last note of the panel, which a short terminal shows only after
/// scrolling.
const ESTIMATE_NOTE: &str = "the provider bill is";

/// Session that `seed_review_ready_session` creates.
const SEEDED_SESSION_ID: &str = "review-shortcut-0001";

/// Asserts that `capture` shows the usage panel of the priced Muse turn.
fn assert_usage_panel(report: &ProofReport, capture: usize) {
    let frame = common::frame_from_capture(&report.captures[capture]);
    let full = Region::full(frame.cols(), frame.rows());
    for text in [
        "Usage · ",
        "Est. cost",
        "Cached",
        "muse-spark-1.3",
        SESSION_TOTAL,
        "Costs are estimated",
        "Esc: close",
    ] {
        assertion::assert_text_in_region(&frame, text, &full);
    }
}

#[tokio::test]
async fn test_usage_panel_shows_per_model_usage_and_closes() -> E2eResult {
    // Arrange
    let server = scripted_provider().await;

    // Act, Assert
    FeatureTest::new("usage_panel")
        .with_git()
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
        .env("MODEL_API_KEY", "test-key")
        .env("MODEL_API_BASE_URL", server.uri())
        .run(
            |scenario| {
                run_priced_turn(scenario)
                    .press_key("u")
                    .wait_for_text(SESSION_TOTAL, 5000)
                    .capture_labeled("usage_panel_key", "u opens the usage panel")
                    .press_key("Esc")
                    .step(common::wait_for_frame_text(&[], &[SESSION_TOTAL]))
                    .press_key("Enter")
                    .step(common::wait_for_prompt_composer())
                    .write_text("/usage")
                    .wait_for_text("token usage and estimated cost", 3000)
                    .press_key("Enter")
                    .wait_for_text(SESSION_TOTAL, 5000)
                    .capture_labeled("usage_panel_slash", "/usage opens the usage panel")
                    .press_key("Esc")
                    .step(common::wait_for_frame_text(&[], &[SESSION_TOTAL]))
                    .step(common::wait_for_prompt_composer())
            },
            |frame, report| {
                Box::pin(async move {
                    assert_usage_panel(report, 0);
                    assert_usage_panel(report, 1);
                    assertion::assert_not_visible(frame, SESSION_TOTAL);
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_usage_panel_scrolls_on_a_short_terminal() -> E2eResult {
    // Arrange, Act, Assert: recorded usage is seeded, so no turn, commit,
    // or review output can move the short screen while the panel opens.
    FeatureTest::new("usage_panel_scroll")
        .with_git()
        .with_terminal_size(80, 16)
        .setup(|env| Box::pin(async move { seed_session_with_recorded_usage(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("u")
                    .wait_for_text(SCROLL_HINT, 5000)
                    .capture_labeled("usage_panel_top", "The short panel offers scrolling")
                    .press_key("pagedown")
                    .wait_for_text(ESTIMATE_NOTE, 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    let top = common::frame_from_capture(&report.captures[0]);
                    assertion::assert_not_visible(&top, ESTIMATE_NOTE);
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, ESTIMATE_NOTE, &full);
                    assertion::assert_text_in_region(frame, SCROLL_HINT, &full);
                })
            },
        )
        .await?;

    Ok(())
}

/// Seeds a review-ready session whose store records priced turn usage of
/// several models plus utility usage, so its panel overflows a short
/// terminal.
async fn seed_session_with_recorded_usage(env: &BuilderEnv) -> E2eResult {
    seed_review_ready_session(env).await?;
    let database = common::open_database(env).await?;
    let usage = database.usage();
    for (model, purpose) in [
        ("model-a", UsagePurpose::Turn),
        ("model-b", UsagePurpose::Turn),
        ("model-c", UsagePurpose::Turn),
        ("model-a", UsagePurpose::Title),
        ("model-a", UsagePurpose::CommitAssist),
    ] {
        let model_usage = ModelUsage {
            cost: Some(CostEstimate {
                basis: CostBasis::ListPrice,
                micro_usd: 12_000,
            }),
            input_tokens: 1_200,
            output_tokens: 300,
            requests: 1,
            ..ModelUsage::new(model)
        };
        usage
            .upsert_session_usage(SEEDED_SESSION_ID, purpose, &TurnUsage::single(model_usage))
            .await?;
    }

    Ok(())
}

/// Starts a session on the priced harness Muse model, runs one turn that
/// reports usage, and waits until its commit message settles the session in
/// session view.
fn run_priced_turn(scenario: Scenario) -> Scenario {
    scenario
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .wait_for_text("Start immediately", 5000)
        .press_key("Enter")
        .step(common::wait_for_prompt_composer())
        .compose(&switch_to_harness_muse_model())
        .write_text("Create notes")
        .wait_for_text("Create notes", 3000)
        .press_key("Enter")
        .eventually(
            Duration::from_secs(30),
            Duration::from_millis(100),
            |frame| {
                let full = Region::full(frame.cols(), frame.rows());

                assertion::match_text_in_region(frame, HARNESS_ANSWER, &full)
            },
        )
        .eventually(
            Duration::from_secs(30),
            Duration::from_millis(100),
            |frame| {
                let full = Region::full(frame.cols(), frame.rows());

                assertion::match_text_in_region(frame, "[Commit] committed", &full)
            },
        )
}
