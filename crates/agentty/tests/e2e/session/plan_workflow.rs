//! One terminal journey through explicit plan acceptance and stage retry.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use testty::assertion;
use testty::frame::TerminalFrame;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common::{self, BuilderEnv, FeatureTest};
use crate::session::fixture::{E2eResult, seed_project_settings};

const WORKFLOW_MODEL_ID: &str = "claude-haiku-4-5-20251001";

async fn seed_workflow_project(env: &BuilderEnv) -> E2eResult {
    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "claude"),
            ("DefaultSmartModel", WORKFLOW_MODEL_ID),
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
case "$prompt" in
  *'Act as the planner'*)
    case "$prompt" in
      *'Include search tests'*) answer='Plan version 2: add indexed search and tests.' ;;
      *) answer='Plan version 1: add indexed search.' ;;
    esac
    ;;
  *'Act as the implementer'*)
    if [ ! -f "$AGENTTY_TEST_IMPLEMENTER_STARTED" ]; then
      : > "$AGENTTY_TEST_IMPLEMENTER_STARTED"
      while [ -d "$stub_root" ]; do sleep 0.02; done
      exit 0
    fi
    answer='Implementation finished from accepted plan version 2.'
    ;;
  *) answer='Session unchanged.' ;;
esac
printf '{"type":"result","subtype":"success","structured_output":{"answer":"%s"},"usage":{"input_tokens":8,"output_tokens":6}}\n' "$answer"
"#,
    )?;
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Wait until cancellation can exercise the running worker rather than queued
/// admission.
fn wait_for_implementer(scenario: Scenario, started: PathBuf) -> Scenario {
    scenario.eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        move |frame| {
            let expected = if started.is_file() {
                "Implementer running"
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

/// Ordinary chat shows the request, live graph, and role-attributed answers.
fn assert_chat_shows_chain(chat: &TerminalFrame) {
    let full = Region::full(chat.cols(), chat.rows());
    for expected in [
        "› Add search",
        "⬡ Execution graph",
        "Planner      ✓ Done",
        "│ accepted plan ✓",
        "Implementer  ✓ Done",
        "◆ Planner",
        "Plan version 2: add indexed search and tests.",
        "◆ Implementer",
        "Implementation finished from accepted plan version 2.",
    ] {
        assertion::assert_text_in_region(chat, expected, &full);
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
                    .capture_labeled("disabled", "Typing /plan cannot open the panel")
            },
            |frame, report| {
                Box::pin(async move {
                    let menu = common::frame_from_capture(&report.captures[0]);
                    assertion::assert_not_visible(&menu, "/plan");
                    assertion::assert_not_visible(frame, "Plan then implement");
                    assertion::assert_not_visible(frame, "New plan");
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
async fn plan_then_implement_requires_acceptance_and_preserves_role_history() -> E2eResult {
    // Arrange, Act, Assert: lower tests own phase and identity permutations.
    let gate = tempfile::tempdir()?;
    let started = gate.path().join("implementation-started");
    FeatureTest::new("plan_then_implement")
        .with_git()
        .with_terminal_size(100, 60)
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
                    .wait_for_text("New plan · original request", 5000)
                    .write_text("Add search")
                    .press_key("Enter")
                    .wait_for_text("Awaiting plan acceptance", 20000)
                    .wait_for_text("Plan version 1", 5000)
                    .capture_labeled(
                        "planner_ready",
                        "Read-only Planner waits for explicit acceptance",
                    )
                    .press_key("r")
                    .wait_for_text("Revise plan · instructions for Planner", 3000)
                    .write_text("Include search tests")
                    .press_key("Enter")
                    .wait_for_text("Plan version 2", 20000)
                    // The answer and settled status refresh the panel
                    // separately; accept only after the last load lands.
                    .wait_for_stable_frame(500, 5000)
                    .press_key("Enter")
                    .wait_for_text("Implementer running", 20000);
                wait_for_implementer(scenario, started)
                    .press_key("Esc")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("T")
                    .wait_for_text("Implementer running", 5000)
                    .press_key("x")
                    .wait_for_text("Canceled · retry after the worker stops", 10000)
                    .press_key("Esc")
                    .wait_for_text("Enter: reply", 10000)
                    .write_text("T")
                    .wait_for_text("Canceled · retry after the worker stops", 5000)
                    .press_key("r")
                    .wait_for_text("Retry Implementer · optional clarification", 3000)
                    .press_key("Enter")
                    .wait_for_text("Implementation succeeded", 20000)
                    .wait_for_text(
                        "Implementation finished from accepted plan version 2.",
                        5000,
                    )
                    .capture_labeled(
                        "implementation_done",
                        "Implementer retry retains the accepted plan",
                    )
                    .press_key("Esc")
                    .wait_for_text("Implementer  ✓ Done", 10000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "session_chat",
                        "Role answers and the live execution graph appear in chat",
                    )
                    .press_key("/")
                    .write_text("plan")
                    .press_key("Enter")
                    .wait_for_text("Implementation succeeded", 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    let planner = common::frame_from_capture(&report.captures[0]);
                    let planner_full = Region::full(planner.cols(), planner.rows());
                    assertion::assert_text_in_region(&planner, "Planner", &planner_full);
                    assertion::assert_text_in_region(&planner, "Plan version 1", &planner_full);
                    assert_chat_shows_chain(&common::frame_from_capture(&report.captures[2]));
                    assertion::assert_text_in_region(
                        frame,
                        "Implementation succeeded",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                })
            },
        )
        .await?;

    Ok(())
}
