//! Preview Harness sessions behind `--experimental-harness`.

use std::os::unix::fs::symlink;
use std::time::Duration;

use serde_json::json;
use testty::assertion;
use testty::region::Region;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::fixture::E2eResult;
use crate::common;
use crate::common::FeatureTest;

/// Answer the scripted provider returns after the write tool succeeds.
const HARNESS_ANSWER: &str = "Created notes.txt through the harness.";
/// Answer the scripted provider returns to the resumed follow-up turn.
const FOLLOW_UP_ANSWER: &str = "The notes say harness.";

/// Returns one terminal session answer.
fn answer(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"answer": text, "questions": []}).to_string()}
        }],
        "usage": {"prompt_tokens": 30, "completion_tokens": 7, "total_tokens": 37}
    }))
}

/// Scripts a provider that writes `notes.txt`, answers, then answers a
/// follow-up turn. The commit-message utility, whose prompt repeats the
/// session requests, gets its own answer.
async fn scripted_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            "Generate the canonical session commit message",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": json!({"answer": "Add harness notes"}).to_string()}
            }]
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("Summarize the notes"))
        .respond_with(answer(FOLLOW_UP_ANSWER))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""tool_call_id":"call-write""#))
        .respond_with(answer(HARNESS_ANSWER))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("Create notes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call-write",
                        "type": "function",
                        "function": {
                            "name": "write",
                            "arguments": json!({
                                "path": "notes.txt",
                                "patch": "--- /dev/null\n+++ b/notes.txt\n@@ -0,0 +1 @@\n+harness\n"
                            }).to_string()
                        }
                    }]
                }
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
        })))
        .with_priority(3)
        .mount(&server)
        .await;

    server
}

#[tokio::test]
async fn test_harness_session_runs_with_flag_and_provider_key() -> E2eResult {
    // Arrange
    let server = scripted_provider().await;

    // Act, Assert
    FeatureTest::new("harness_session")
        .with_git()
        .args(["--experimental-harness".to_string()])
        .env("MODEL_API_KEY", "test-key")
        .env("MODEL_API_BASE_URL", server.uri())
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("[Preview] Built-in agent", 5000)
                    .press_key("j")
                    .press_key("j")
                    .press_key("j")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
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
                    .capture_labeled("harness_answer", "Harness session answered")
                    // The commit message runs on the session's Harness model.
                    .eventually(
                        Duration::from_secs(30),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());

                            assertion::match_text_in_region(frame, "[Commit] committed", &full)
                        },
                    )
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Summarize the notes")
                    .wait_for_text("Summarize the notes", 3000)
                    .press_key("Enter")
                    .eventually(
                        Duration::from_secs(30),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());

                            assertion::match_text_in_region(frame, FOLLOW_UP_ANSWER, &full)
                        },
                    )
                    .capture_labeled("harness_follow_up", "Harness session resumed")
            },
            |frame, _report| {
                Box::pin(async move {
                    // Both answers were awaited above; later commit output can
                    // scroll them, so the final frame checks stable session
                    // facts.
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Lines: +1 / -0", &full);
                    assertion::assert_text_in_region(frame, "harness/muse-spark-1.3", &full);
                })
            },
        )
        .await?;

    let requests = server.received_requests().await.unwrap_or_default();
    let follow_up = requests
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .find(|body| body.contains("Summarize the notes"))
        .expect("the follow-up turn should reach the provider");
    assert!(follow_up.contains(HARNESS_ANSWER));
    // Only the durable harness history replays the earlier tool exchange;
    // Agentty's transcript fallback carries the answer text alone.
    assert!(follow_up.contains(r#""tool_call_id":"call-write""#));

    Ok(())
}

#[tokio::test]
async fn test_harness_row_is_hidden_without_flag() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("harness_hidden")
        .with_git()
        .env("MODEL_API_KEY", "test-key")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Orchestrator", 5000)
                    .capture_labeled("session_types", "Session types without Harness")
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Orchestrator", &full);
                    assertion::assert_not_visible(frame, "Harness");
                })
            },
        )
        .await
}

/// Returns a directory exposing only the host `git`, so a launch using it as
/// `PATH` has Harness as its sole backend.
///
/// It lives outside the fixture root, whose guard `.git` would otherwise make
/// the harness treat the executable as part of the worktree.
fn git_only_path() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let git_directory = tempfile::tempdir()?;
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join("git"))
        .find(|path| path.is_file())
        .ok_or("git not found on PATH")?;
    symlink(real_git, git_directory.path().join("git"))?;

    Ok(git_directory)
}

#[tokio::test]
async fn test_harness_only_launch_defaults_to_harness_session() -> E2eResult {
    // Arrange
    let server = scripted_provider().await;
    let git_directory = git_only_path()?;

    // Act, Assert
    FeatureTest::new("harness_only")
        .with_git()
        .with_stub_only_path()
        .env("PATH", git_directory.path().to_string_lossy())
        .args(["--experimental-harness".to_string()])
        .env("MODEL_API_KEY", "test-key")
        .env("MODEL_API_BASE_URL", server.uri())
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Install an agent CLI", 5000)
                    .capture_labeled("harness_only_types", "Only Harness is selectable")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
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
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "harness/muse-spark-1.3", &full);
                })
            },
        )
        .await
}
