//! One regular-chat journey through the orchestrated Planner → Implementer
//! chain: questions, revision, natural-language approval, cancel, retry, and
//! follow-up.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use testty::assertion;
use testty::frame::TerminalFrame;
use testty::proof::report::ProofReport;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common::{self, BuilderEnv, FeatureTest};
use crate::session::fixture::{E2eResult, seed_project_settings};

const WORKFLOW_MODEL_ID: &str = "claude-haiku-4-5-20251001";

const IMPLEMENTER_REVIEW_TEXT: &str = "Implementer focused review completed.";
const CLEAN_REVIEW_TEXT: &str = "No suggestions remain after implementation review.";

/// Seeds a scripted Claude provider for every chain role and the
/// orchestrator decision.
///
/// The Planner first asks a question, then plans from the answer and
/// revision. The decision approves only "start implementing". The first
/// Implementer run blocks until canceled; later runs finish immediately.
async fn seed_workflow_project(env: &BuilderEnv) -> E2eResult {
    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "claude"),
            ("DefaultSmartModel", WORKFLOW_MODEL_ID),
            ("DefaultReviewAgent", "claude"),
            ("DefaultReviewModel", WORKFLOW_MODEL_ID),
        ],
    )
    .await?;
    let provider = env.stub_bin.join("claude");
    std::fs::write(
        &provider,
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
prompt=$(cat)
stub_root=$(dirname "$0")
printf '%s\n' '{"type":"system","subtype":"init"}'
questions=''
case "$prompt" in
  *'Review the Git diff for display in a terminal UI.'*)
    if grep -q 'reviewed' search.txt 2>/dev/null; then
      result='{\"project_impact\":[\"No suggestions remain after implementation review.\"],\"suggestions\":[]}'
    else
      result='{\"project_impact\":[\"Implementer focused review completed.\"],\"suggestions\":[{\"details\":\"Mark the indexed search implementation as reviewed.\",\"severity\":\"medium\"}],\"candidate_decisions\":[{\"candidate_index\":0,\"suggestion_index\":0,\"reason\":\"The fixture search implementation needs review.\"}]}'
    fi
    printf '%s\n' "{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"$result\",\"usage\":{\"input_tokens\":5,\"output_tokens\":9}}"
    exit 0
    ;;
  *"Decide whether the user's reply approves"*)
    case "$prompt" in
      *'start implementing'*) answer='implement' ;;
      *) answer='revise' ;;
    esac
    ;;
  *'Act as the planner'*)
    case "$prompt" in
      *'Include search tests'*) answer='Plan version 2: index titles and add search tests.' ;;
      *'Use the title index'*) answer='Plan version 1: index titles.' ;;
      *)
        answer=''
        questions=',"questions":[{"text":"Which index should search use?","options":[]}]'
        ;;
    esac
    ;;
  *'Act as the implementer'*)
    if [ ! -f "$AGENTTY_TEST_IMPLEMENTER_STARTED" ]; then
      : > "$AGENTTY_TEST_IMPLEMENTER_STARTED"
      while [ -d "$stub_root" ]; do sleep 0.02; done
      exit 0
    fi
    case "$prompt" in
      *'Also update the docs'*)
        printf 'Indexed search documentation\n' > search-guide.txt
        answer='Docs updated for indexed search.'
        ;;
      *'Verify the focused-review suggestions against the current code'*)
        printf 'reviewed indexed search\n' > search.txt
        answer='Review suggestion applied by the Implementer.'
        ;;
      *)
        printf 'indexed search\n' > search.txt
        answer='Implementation finished from accepted plan version 2.'
        ;;
    esac
    ;;
  *) answer='Session unchanged.' ;;
esac
printf '{"type":"result","subtype":"success","structured_output":{"answer":"%s"%s},"usage":{"input_tokens":8,"output_tokens":6}}\n' "$answer" "$questions"
"#,
    )?;
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Footer text shown only while the composer owns input.
const COMPOSER_FOOTER: &str = "Enter: send";

/// Opens the composer, sends `message`, and returns to the chat view.
fn send_chat_message(scenario: Scenario, message: &str) -> Scenario {
    open_composer(scenario)
        .write_text(message)
        .press_key("Enter")
}

/// Opens the composer from the chat view.
fn open_composer(scenario: Scenario) -> Scenario {
    scenario
        .press_key("Enter")
        .wait_for_text(COMPOSER_FOOTER, 5000)
}

/// Waits until the scripted Implementer process runs, so cancellation
/// exercises the running worker rather than queued admission.
fn wait_for_implementer(scenario: Scenario, started: PathBuf) -> Scenario {
    scenario.eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        move |frame| {
            let expected = if started.is_file() {
                "Implementer  ◐ Working"
            } else {
                "Scripted Implementer startup marker"
            };
            assertion::match_text_in_region(
                frame,
                expected,
                &Region::full(frame.cols(), frame.rows()),
            )
        },
    )
}

/// Asserts each expected text is visible in one captured frame.
fn assert_visible(frame: &TerminalFrame, expected: &[&str]) {
    let full = Region::full(frame.cols(), frame.rows());
    for text in expected {
        assertion::assert_text_in_region(frame, text, &full);
    }
}

#[tokio::test]
async fn plan_command_is_unavailable_by_default() -> E2eResult {
    // Arrange, Act, Assert: prove the default launch hides and rejects `/plan`.
    FeatureTest::new("plan_command_disabled")
        .with_git()
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 10000)
                    .press_key("/")
                    .wait_for_text("Slash Command", 3000)
                    .capture_labeled("commands", "Planning is absent from the default menu")
                    .write_text("plan")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 3000)
                    .capture_labeled("disabled", "Typing /plan cannot arm the composer")
            },
            |frame, report| {
                Box::pin(async move {
                    let menu = common::frame_from_capture(&report.captures[0]);
                    assertion::assert_not_visible(&menu, "/plan");
                    assertion::assert_not_visible(frame, "Plan then implement");
                    assertion::assert_not_visible(frame, "describe the task for the Planner");
                    assertion::assert_text_in_region(
                        frame,
                        "/plan",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn plan_then_implement_runs_as_an_orchestrated_chat() -> E2eResult {
    // Arrange, Act, Assert: lower tests own routing and phase permutations.
    let gate = tempfile::tempdir()?;
    let started = gate.path().join("implementation-started");
    FeatureTest::new("plan_then_implement")
        .with_git()
        .with_terminal_size(100, 80)
        .args(["--experimental-multi-agent".to_string()])
        .env(
            "AGENTTY_TEST_IMPLEMENTER_STARTED",
            started.to_string_lossy(),
        )
        .setup(|env| Box::pin(async move { seed_workflow_project(env).await }))
        .run(
            move |scenario| {
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 10000)
                    .press_key("/")
                    .write_text("plan")
                    .wait_for_text("Plan then implement", 3000)
                    .press_key("Enter")
                    .wait_for_text("Plan · describe the task for the Planner", 5000)
                    .capture_labeled("armed", "/plan arms the regular composer")
                    .press_key("Esc")
                    .wait_for_text("Type your message", 5000)
                    .write_text("/plan /src/search.rs needs indexed search")
                    .press_key("Enter")
                    .wait_for_text("Which index should search use?", 20000)
                    .wait_for_text("Planner      ? Needs your input", 10000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("question", "The Planner asks in the chat");
                let scenario = send_chat_message(scenario, "Use the title index")
                    .wait_for_text("Plan version 1: index titles.", 20000)
                    .wait_for_text("Planner      ● Awaiting your approval", 10000)
                    .wait_for_stable_frame(300, 5000);
                let scenario = open_composer(scenario)
                    .wait_for_text("tell the orchestrator to start implementing", 5000)
                    .capture_labeled(
                        "awaiting_approval",
                        "The composer shows the orchestrator state",
                    )
                    .write_text("Include search tests")
                    .press_key("Enter")
                    .wait_for_text("Plan version 2: index titles and add search tests.", 20000)
                    .wait_for_text("Planner      ● Awaiting your approval", 10000)
                    .wait_for_stable_frame(300, 5000);
                let scenario = send_chat_message(scenario, "Looks good, start implementing")
                    .wait_for_text("Plan accepted · Implementer started", 20000);
                let scenario = wait_for_implementer(scenario, started)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("implementing", "Planner done, Implementer working");
                let scenario = send_chat_message(scenario, "Hurry up")
                    .wait_for_text("The Implementer is still working", 10000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("busy", "A reply while the Implementer works is kept")
                    .press_key("Esc")
                    .wait_for_text("Ctrl+c: stop", 10000)
                    .press_key("ctrl+c")
                    .wait_for_text("Implementer  ⊘ Canceled", 20000)
                    .wait_for_text("Enter: reply", 20000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("canceled", "Ctrl+c cancels the running Implementer");
                complete_reviewed_implementation(scenario)
            },
            |frame, report| {
                Box::pin(async move {
                    assert_orchestrated_chat(frame, report);
                })
            },
        )
        .await?;

    Ok(())
}

/// Finishes the retry, applies focused-review feedback, and sends a follow-up.
fn complete_reviewed_implementation(scenario: Scenario) -> Scenario {
    let scenario = send_chat_message(scenario, "Retry please")
        .wait_for_text(IMPLEMENTER_REVIEW_TEXT, 30000)
        .wait_for_text("Implementer  ✓ Done", 10000)
        .wait_for_stable_frame(300, 5000)
        .capture_labeled("implemented", "The Implementer starts focused review");
    let scenario = open_composer(scenario)
        .write_text("/apply")
        .press_key("Enter")
        .wait_for_text("Review suggestion applied by the Implementer.", 30000)
        .wait_for_text(CLEAN_REVIEW_TEXT, 30000)
        .wait_for_stable_frame(300, 5000)
        .capture_labeled(
            "applied",
            "/apply routes review feedback to the Implementer",
        );
    let scenario = send_chat_message(scenario, "Also update the docs")
        .wait_for_text("Docs updated for indexed search.", 20000)
        .wait_for_text(CLEAN_REVIEW_TEXT, 30000)
        .wait_for_stable_frame(300, 5000);

    open_composer(scenario)
        .wait_for_text("Ask the Implementer for more changes", 5000)
        .wait_for_stable_frame(300, 5000)
        .capture_labeled("follow_up", "Follow-ups reach the Implementer")
}

/// Checks the visible conversation states captured by the chat journey.
fn assert_orchestrated_chat(frame: &TerminalFrame, report: &ProofReport) {
    let capture = |index: usize| common::frame_from_capture(&report.captures[index]);
    assert_visible(
        &capture(0),
        &[
            "Plan · describe the task for the Planner",
            "Describe the task for the Planner",
        ],
    );
    assert_visible(
        &capture(1),
        &[
            "› /src/search.rs needs indexed search",
            "⬡ Execution graph",
            "◆ Planner",
            "Which index should search use?",
            "Orchestrator · Planner · Needs your input",
        ],
    );
    assert_visible(
        &capture(2),
        &["Orchestrator · Planner · Awaiting your approval"],
    );
    assert_visible(
        &capture(3),
        &[
            "Planner      ✓ Done",
            "│ accepted plan ✓",
            "Implementer  ◐ Working",
            "Plan version 2: index titles and add search tests.",
        ],
    );
    assert_visible(
        &capture(4),
        &[
            "The Implementer is still working",
            "Hurry up",
            "Orchestrator · Implementer · Working",
        ],
    );
    assert_visible(&capture(5), &["Implementer  ⊘ Canceled"]);
    assert_visible(
        &capture(6),
        &[
            "◆ Implementer",
            "Implementation finished from accepted plan version 2.",
            "Implementer  ✓ Done",
            IMPLEMENTER_REVIEW_TEXT,
        ],
    );
    assert_visible(
        &capture(7),
        &[
            "Review suggestion applied by the Implementer.",
            CLEAN_REVIEW_TEXT,
        ],
    );
    assert_visible(
        frame,
        &[
            "› Also update the docs",
            "Docs updated for indexed search.",
            "Orchestrator · Implementer · Done",
        ],
    );
}
