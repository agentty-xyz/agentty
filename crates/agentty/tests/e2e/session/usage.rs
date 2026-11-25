//! The per-session usage panel, opened with `u` and `/usage`, its marks for
//! upper bounds and plan-billed API equivalents, and the live usage readout
//! beside the composer timer.

use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use ag_contracts::{CostBasis, CostEstimate, ModelUsage, PricedCost, TurnUsage, UsagePurpose};
use ag_session::test_support as model_fixture;
use serde_json::json;
use testty::assertion;
use testty::proof::report::ProofReport;
use testty::region::Region;
use testty::scenario::Scenario;

use super::fixture::{E2eResult, seed_project_settings, seed_review_ready_session};
use super::harness::{
    HARNESS_ANSWER, NO_LOCAL_SERVER, TITLE_PROMPT, read_request_body, scripted_provider,
    switch_to_harness_muse_model,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, ReleaseGate};

/// Row that closes the usage table.
const SESSION_TOTAL: &str = "Session total";

/// Answer of the Codex stub signed in with a `ChatGPT` plan.
const PLAN_ANSWER: &str = "Codex answered on a ChatGPT plan.";

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
            cost: Some(CostEstimate::new(CostBasis::ListPrice, 12_000)),
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

#[tokio::test]
async fn test_usage_panel_marks_cache_reads_without_a_cache_price_as_upper_bounds() -> E2eResult {
    // Arrange, Act, Assert: the upper-bound cost is seeded, so the scenario
    // does not depend on which cataloged models declare a cache price.
    FeatureTest::new("usage_panel_upper_bound")
        .with_git()
        .with_terminal_size(100, 40)
        .setup(|env| Box::pin(async move { seed_session_with_upper_bound_usage(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("u")
                    .wait_for_text(SESSION_TOTAL, 5000)
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    for text in [
                        "≤",
                        "≤ Upper bound:",
                        "75% cached, no cache price for qwen3.8-max",
                    ] {
                        assertion::assert_text_in_region(frame, text, &full);
                    }
                })
            },
        )
        .await?;

    Ok(())
}

/// Seeds a review-ready session whose store records one turn on a model
/// without a cache price: its cache reads were priced at the input price, so
/// the recorded cost is an upper bound.
async fn seed_session_with_upper_bound_usage(env: &BuilderEnv) -> E2eResult {
    seed_review_ready_session(env).await?;
    let database = common::open_database(env).await?;
    let model_usage = ModelUsage {
        cache_read_tokens: Some(900),
        cost: Some(CostEstimate::charged(PricedCost {
            upper_bound: true,
            ..PricedCost::new(CostBasis::ListPrice, 4_200)
        })),
        input_tokens: 1_200,
        output_tokens: 300,
        requests: 1,
        ..ModelUsage::new("qwen3.8-max")
    };
    database
        .usage()
        .upsert_session_usage(
            SEEDED_SESSION_ID,
            UsagePurpose::Turn,
            &TurnUsage::single(model_usage),
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_usage_panel_marks_chatgpt_plan_costs_as_api_equivalents() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("usage_panel_plan_login")
        .with_git()
        .with_terminal_size(120, 40)
        .setup(|env| Box::pin(async move { seed_codex_plan_project(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("· Normal", 5000)
                    .write_text("Answer on the plan")
                    .press_key("Enter")
                    .wait_for_text(PLAN_ANSWER, 30000)
                    .press_key("u")
                    .wait_for_text(SESSION_TOTAL, 5000)
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    for text in [
                        "ChatGPT subscription",
                        "≈$",
                        "API-equivalent, plan-billed",
                        "≈ API-equivalent: ChatGPT plan login; priced at OpenAI API list prices",
                        SESSION_TOTAL,
                    ] {
                        assertion::assert_text_in_region(frame, text, &full);
                    }
                })
            },
        )
        .await?;

    Ok(())
}

/// Prompt of the live-readout scenario.
const READOUT_PROMPT: &str = "Write two notes";

/// Answer of the live-readout scenario's last request.
const READOUT_ANSWER: &str = "Wrote both notes.";

/// Context fill the status-line readout shows after the first model
/// request, with the turn charge that follows it.
const FIRST_REQUEST_READOUT: &str = "▱ 12% · $0.05 turn";

/// Context fill and turn charge the status-line readout shows after the
/// second model request: each request's uncached input and output at Muse
/// Spark's list prices, and its cache reads at the cached-input price.
const SECOND_REQUEST_READOUT: &str = "▱ 13% · $0.11 turn";

/// Session figures the composer border shows beside the timer.
const SESSION_READOUT: &str = " session │ Timer:";

#[tokio::test]
async fn test_usage_readout_updates_after_each_model_request() -> E2eResult {
    // Arrange
    let second_request = ReleaseGate::new()?;
    let third_request = ReleaseGate::new()?;
    let server = gated_readout_provider(second_request.clone(), third_request.clone())?;

    // Act, Assert
    FeatureTest::new("usage_readout")
        .with_git()
        // The status line keeps a readout slot only on panes wider than 80
        // columns.
        .with_terminal_size(130, 30)
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
        .env("MODEL_API_KEY", "test-key")
        .env("MODEL_API_BASE_URL", server)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Start immediately", 5000)
                    .press_key("Enter")
                    .step(common::wait_for_prompt_composer())
                    .compose(&switch_to_harness_muse_model())
                    .write_text(READOUT_PROMPT)
                    .wait_for_text(READOUT_PROMPT, 3000)
                    .press_key("Enter")
                    // The provider holds the second request, so the turn
                    // runs with the first request's usage.
                    .wait_for_text(FIRST_REQUEST_READOUT, 30000)
                    .capture_labeled("readout_first_request", "Readout after one request")
                    .compose(&second_request.release())
                    .wait_for_text(SECOND_REQUEST_READOUT, 30000)
                    .capture_labeled("readout_second_request", "Readout after two requests")
                    .compose(&third_request.release())
                    .wait_for_text(READOUT_ANSWER, 30000)
                    .step(common::wait_for_frame_text(
                        &[SESSION_READOUT],
                        &[SECOND_REQUEST_READOUT],
                    ))
            },
            |frame, report| {
                Box::pin(async move {
                    // While the turn runs, its readout sits in the status
                    // line and the composer border keeps only the timer.
                    for (capture, readout, absent) in [
                        (0, FIRST_REQUEST_READOUT, SECOND_REQUEST_READOUT),
                        (1, SECOND_REQUEST_READOUT, "▱ 12% · "),
                    ] {
                        let running = common::frame_from_capture(&report.captures[capture]);
                        let full = Region::full(running.cols(), running.rows());
                        for text in [readout, "ctx ▰", " turn"] {
                            assertion::assert_text_in_region(&running, text, &full);
                        }
                        assertion::assert_not_visible(&running, absent);
                        assertion::assert_not_visible(&running, SESSION_READOUT);
                    }
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "ctx 14% · $", &full);
                    assertion::assert_text_in_region(frame, SESSION_READOUT, &full);
                    assertion::assert_not_visible(frame, "$0.11 turn");
                })
            },
        )
        .await?;

    Ok(())
}

/// Serves Chat Completions for the live-readout scenario on a local port
/// and returns its base URL.
///
/// The session turn's first request gets a write call; the request carrying
/// its result waits for `second_request` and gets a second write call; the
/// request carrying that result waits for `third_request` and gets the
/// answer. Each request reports cached input, so the turn's tokens grow
/// only after a request completes. The commit-message utility gets its own
/// answer and every other request `404 Not Found`.
fn gated_readout_provider(
    second_request: ReleaseGate,
    third_request: ReleaseGate,
) -> std::io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (second_request, third_request) = (second_request.clone(), third_request.clone());
            std::thread::spawn(move || {
                serve_gated_readout(stream, &second_request, &third_request);
            });
        }
    });

    Ok(format!("http://{address}"))
}

/// Answers one provider connection for [`gated_readout_provider`].
fn serve_gated_readout(
    mut stream: TcpStream,
    second_request: &ReleaseGate,
    third_request: &ReleaseGate,
) {
    let Some(body) = read_request_body(&mut stream) else {
        return;
    };
    let response = if body.contains("Generate the canonical session commit message") {
        answer_completion(&json!({"answer": "Add two notes"}), (100, 0, 10))
    } else if body.contains(r#""tool_call_id":"call-second""#) {
        // A scenario that never opens the gate sees the request fail.
        if !third_request.wait_blocking(Duration::from_secs(60)) {
            return;
        }

        answer_completion(
            &json!({"answer": READOUT_ANSWER, "questions": []}),
            (148_000, 131_000, 1_400),
        )
    } else if body.contains(r#""tool_call_id":"call-first""#) {
        if !second_request.wait_blocking(Duration::from_secs(60)) {
            return;
        }

        write_call_completion("call-second", "second.txt", (139_400, 118_000, 2_100))
    } else if body.contains(READOUT_PROMPT) && !body.contains(TITLE_PROMPT) {
        write_call_completion("call-first", "first.txt", (121_600, 96_000, 1_300))
    } else {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");

        return;
    };
    let body = response.to_string();
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \
             {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
}

/// Returns a completion calling `write` on `path` as `call_id`, reporting
/// `(prompt, cached, completion)` tokens.
fn write_call_completion(call_id: &str, path: &str, tokens: (u64, u64, u64)) -> serde_json::Value {
    json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": "write",
                        "arguments": json!({
                            "path": path,
                            "patch": format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+note\n")
                        }).to_string()
                    }
                }]
            }
        }],
        "usage": completion_usage(tokens)
    })
}

/// Returns a terminal completion with `content`, reporting
/// `(prompt, cached, completion)` tokens.
fn answer_completion(content: &serde_json::Value, tokens: (u64, u64, u64)) -> serde_json::Value {
    json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": content.to_string()}
        }],
        "usage": completion_usage(tokens)
    })
}

/// Returns Chat Completions usage of `(prompt, cached, completion)` tokens.
fn completion_usage((prompt, cached, completion): (u64, u64, u64)) -> serde_json::Value {
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "prompt_tokens_details": {"cached_tokens": cached}
    })
}

/// Installs a Codex app-server stub signed in with a `ChatGPT` plan account,
/// whose turns report token usage with cached input.
async fn seed_codex_plan_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let codex_path = env.stub_bin.join("codex");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'codex-cli 0.146.0\n'; exit 0; fi

extract_id() {
    printf '%s\n' "$1" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'
}

while IFS= read -r request; do
    request_id=$(extract_id "$request")
    case "$request" in
        *'"method":"initialize"'*)
            printf '{"id":"%s","result":{}}\n' "$request_id"
            ;;
        *'"method":"thread/start"'*)
            printf '{"id":"%s","result":{"thread":{"id":"plan-thread"}}}\n' "$request_id"
            ;;
        *'"method":"account/read"'*)
            printf '{"id":"%s","result":{"account":{"type":"chatgpt","email":null,"planType":"plus","workspaceName":null},"requiresOpenaiAuth":true}}\n' "$request_id"
            ;;
        *'"method":"turn/start"'*)
            answer='Codex answered on a ChatGPT plan.'
            case "$request" in
                *'Generate a concise, commit-style title'*) answer='Plan usage' ;;
            esac
            printf '{"id":"%s","result":{"turn":{"id":"plan-turn"}}}\n' "$request_id"
            printf '%s\n' '{"method":"turn/started","params":{"turn":{"id":"plan-turn"}}}'
            printf '%s\n' '{"method":"thread/tokenUsage/updated","params":{"threadId":"plan-thread","turnId":"plan-turn","tokenUsage":{"total":{"inputTokens":120000,"outputTokens":2000,"cachedInputTokens":90000},"last":{"inputTokens":120000,"outputTokens":2000},"modelContextWindow":400000}}}'
            printf '{"method":"item/completed","params":{"threadId":"plan-thread","turnId":"plan-turn","item":{"type":"agentMessage","id":"plan-answer","text":"{\\"answer\\":\\"%s\\"}","phase":"final_answer"}}}\n' "$answer"
            printf '%s\n' '{"method":"turn/completed","params":{"threadId":"plan-thread","turn":{"id":"plan-turn","status":"completed","items":[]}}}'
            ;;
    esac
done
"#;
    std::fs::write(&codex_path, script)?;
    std::fs::set_permissions(&codex_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "codex"),
            ("DefaultSmartModel", model_fixture::CODEX_MODEL_ID),
            ("DefaultFastAgent", "codex"),
            ("DefaultFastModel", model_fixture::CODEX_MODEL_ID),
        ],
    )
    .await
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
