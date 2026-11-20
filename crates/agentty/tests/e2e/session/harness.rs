//! Preview Harness sessions behind `--experimental-harness`.

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::Duration;

use serde_json::json;
use testty::assertion;
use testty::frame::CellColor;
use testty::journey::Journey;
use testty::proof::report::ProofReport;
use testty::region::Region;
use testty::step::Step;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::fixture::E2eResult;
use crate::common;
use crate::common::{FeatureTest, ReleaseGate, SettingsRow};

/// Answer the scripted provider returns after the write tool succeeds.
const HARNESS_ANSWER: &str = "Created notes.txt through the harness.";
/// Answer the scripted provider returns to the resumed follow-up turn.
const FOLLOW_UP_ANSWER: &str = "The notes say harness.";
/// Project impact the scripted provider returns for focused review.
const REVIEW_IMPACT: &str = "Harness reviewed the notes.";
/// Local server endpoint nothing listens on, so a host's own Ollama never
/// joins a scenario's harness models.
const NO_LOCAL_SERVER: &str = "http://127.0.0.1:9/v1";

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
/// follow-up turn. The commit-message and focused-review utilities, whose
/// prompts repeat the session requests, get their own answers.
async fn scripted_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            "Review the Git diff for display in a terminal UI.",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": json!({
                    "candidate_decisions": [],
                    "project_impact": [REVIEW_IMPACT],
                    "suggestions": []
                }).to_string()}
            }]
        })))
        .with_priority(1)
        .mount(&server)
        .await;
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
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
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
                    .press_key("Enter")
                    .step(common::wait_for_prompt_composer())
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
                    .step(common::wait_for_prompt_composer())
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
                    assertion::assert_text_in_region(frame, "(+1 -0)", &full);
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

/// Answer the scripted provider returns after its invented tool call is
/// answered as unavailable.
const UNAVAILABLE_TOOL_ANSWER: &str = "Answered without the missing tool.";

/// Scripts a provider that first calls `read_file`, a tool the harness does
/// not have, then answers once the harness returns that call's result.
async fn unavailable_tool_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""tool_call_id":"call-invented""#))
        .respond_with(answer(UNAVAILABLE_TOOL_ANSWER))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("Inspect the notes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call-invented",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": json!({"file": "notes.txt"}).to_string()
                        }
                    }]
                }
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
        })))
        .with_priority(2)
        .mount(&server)
        .await;

    server
}

#[tokio::test]
async fn test_harness_session_continues_after_an_unavailable_tool_call() -> E2eResult {
    // Arrange
    let server = unavailable_tool_provider().await;

    // Act, Assert
    FeatureTest::new("harness_unavailable_tool")
        .with_git()
        .args(["--experimental-harness".to_string()])
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
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
                    .press_key("Enter")
                    .step(common::wait_for_prompt_composer())
                    .write_text("Inspect the notes")
                    .wait_for_text("Inspect the notes", 3000)
                    .press_key("Enter")
                    .eventually(
                        Duration::from_secs(30),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());

                            assertion::match_text_in_region(frame, UNAVAILABLE_TOOL_ANSWER, &full)
                        },
                    )
                    .capture_labeled(
                        "harness_unavailable_tool_answer",
                        "Harness session answered after an unavailable tool",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, UNAVAILABLE_TOOL_ANSWER, &full);
                })
            },
        )
        .await?;

    let requests = server.received_requests().await.unwrap_or_default();
    let answered = requests
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .find(|body| body.contains(r#""tool_call_id":"call-invented""#))
        .expect("the unavailable call's result should reach the provider");
    assert!(answered.contains("is not available in this turn"));

    Ok(())
}

#[tokio::test]
async fn test_harness_session_notes_a_temporary_provider_failure() -> E2eResult {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "7"))
        .mount(&server)
        .await;

    // Act, Assert
    FeatureTest::new("harness_temporary_failure")
        .with_git()
        .args(["--experimental-harness".to_string()])
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
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
                    .press_key("Enter")
                    .step(common::wait_for_prompt_composer())
                    .write_text("Create notes")
                    .wait_for_text("Create notes", 3000)
                    .press_key("Enter")
                    .eventually(
                        Duration::from_secs(30),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());

                            assertion::match_text_in_region(frame, "(overloaded);", &full)
                        },
                    )
                    .capture_labeled(
                        "harness_temporary_failure",
                        "Harness session noted a temporary provider failure",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Harness turn failed", &full);
                    assertion::assert_text_in_region(frame, "(overloaded);", &full);
                })
            },
        )
        .await?;

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
                    .wait_for_text("Stage locally first", 5000)
                    .capture_labeled("session_types", "Session types without Harness")
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Draft", &full);
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
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
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
                    .step(common::wait_for_prompt_composer())
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
                    // Without CLIs, focused review runs on the session's
                    // Harness model.
                    .eventually(
                        Duration::from_secs(30),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());

                            assertion::match_text_in_region(frame, REVIEW_IMPACT, &full)
                        },
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "harness/muse-spark-1.3", &full);
                    assertion::assert_not_visible(frame, "Review assist unavailable");
                })
            },
        )
        .await
}

/// Verify that creation options disabled without an agent CLI use readable
/// gray in the default theme.
#[tokio::test]
async fn session_creation_default_theme_disabled_options() -> E2eResult {
    // Arrange
    let git_directory = git_only_path()?;

    // Act, Assert
    FeatureTest::new("session_creation_default_theme_disabled_options")
        .env("NO_COLOR", "")
        .with_git()
        .with_stub_only_path()
        .env("PATH", git_directory.path().to_string_lossy())
        .args(["--experimental-harness".to_string()])
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
        .env("MODEL_API_KEY", "test-key")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Install an agent CLI", 5000)
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, _report| {
                Box::pin(async move {
                    let disabled_color = CellColor::new(192, 192, 192);
                    for text in ["Regular", "Install an agent CLI"] {
                        assertion::assert_text_has_fg_color(frame, text, &disabled_color);
                        assertion::assert_text_has_bg_color(frame, text, &CellColor::black());
                    }
                    assertion::assert_span_is_highlighted(frame, "Harness");
                })
            },
        )
        .await
}

#[tokio::test]
async fn test_harness_session_runs_on_a_local_openai_compatible_server() -> E2eResult {
    // Arrange
    let server = scripted_provider().await;
    let git_directory = git_only_path()?;

    // Act, Assert
    FeatureTest::new("harness_local_model")
        .with_git()
        .with_stub_only_path()
        .env("PATH", git_directory.path().to_string_lossy())
        .args(["--experimental-harness".to_string()])
        .env("DASHSCOPE_API_KEY", "")
        .env("KIMI_API_KEY", "")
        .env("MODEL_API_KEY", "")
        .env("OPENAI_API_KEY", "")
        .env("OPENAI_COMPATIBLE_BASE_URL", server.uri())
        .env("OPENAI_COMPATIBLE_MODEL", "qwen3:latest")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Install an agent CLI", 5000)
                    .press_key("Enter")
                    .step(common::wait_for_prompt_composer())
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
                    assertion::assert_text_in_region(frame, "harness/openai-compatible", &full);
                })
            },
        )
        .await?;

    let requests = server.received_requests().await.unwrap_or_default();
    let session_request = requests
        .iter()
        .find(|request| String::from_utf8_lossy(&request.body).contains("Create notes"))
        .ok_or("the session turn should reach the local server")?;
    assert!(!session_request.headers.contains_key("authorization"));
    assert!(String::from_utf8_lossy(&session_request.body).contains(r#""model":"qwen3:latest""#));

    Ok(())
}

/// Returns one streamed Responses API response completing with `items`.
fn responses_stream(items: &[serde_json::Value]) -> ResponseTemplate {
    let mut events = items
        .iter()
        .map(|item| json!({"type": "response.output_item.done", "item": item}))
        .collect::<Vec<_>>();
    events.push(json!({
        "type": "response.completed",
        "response": {
            "id": "resp_e2e",
            "status": "completed",
            "usage": {"input_tokens": 30, "output_tokens": 7, "total_tokens": 37}
        }
    }));
    let body = events.iter().fold(String::new(), |mut body, event| {
        let _ = write!(body, "data: {event}\n\n");

        body
    });

    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

/// Returns one assistant message item carrying `output` as JSON text.
fn responses_message(output: &serde_json::Value) -> serde_json::Value {
    json!({
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": output.to_string()}]
    })
}

/// Scripts an `OpenAI` Responses server that writes `notes.txt` and answers,
/// with the commit-message utility, whose prompt repeats the session
/// request, answered on its own.
async fn scripted_openai_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_string_contains(
            "Generate the canonical session commit message",
        ))
        .respond_with(responses_stream(&[responses_message(
            &json!({"answer": "Add harness notes"}),
        )]))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_string_contains(r#""type":"function_call_output""#))
        .respond_with(responses_stream(&[responses_message(
            &json!({"answer": HARNESS_ANSWER, "questions": []}),
        )]))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_string_contains("Create notes"))
        .respond_with(responses_stream(&[
            json!({
                "type": "reasoning",
                "id": "rs_e2e",
                "summary": [],
                "encrypted_content": "gAAAA-e2e"
            }),
            json!({
                "type": "function_call",
                "call_id": "call-write",
                "name": "write",
                "arguments": json!({
                    "path": "notes.txt",
                    "patch": "--- /dev/null\n+++ b/notes.txt\n@@ -0,0 +1 @@\n+harness\n"
                })
                .to_string()
            }),
        ]))
        .with_priority(3)
        .mount(&server)
        .await;

    server
}

#[tokio::test]
async fn test_harness_session_runs_on_openai_with_an_api_key() -> E2eResult {
    // Arrange
    let server = scripted_openai_provider().await;
    let git_directory = git_only_path()?;

    // Act, Assert
    FeatureTest::new("harness_openai_model")
        .with_git()
        .with_stub_only_path()
        .env("PATH", git_directory.path().to_string_lossy())
        .args(["--experimental-harness".to_string()])
        .env("DASHSCOPE_API_KEY", "")
        .env("KIMI_API_KEY", "")
        .env("MODEL_API_KEY", "")
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
        .env("OPENAI_COMPATIBLE_MODEL", "")
        .env("OPENAI_API_KEY", "test-openai-key")
        .env("OPENAI_BASE_URL", format!("{}/v1", server.uri()))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Install an agent CLI", 5000)
                    .press_key("Enter")
                    .step(common::wait_for_prompt_composer())
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
                    assertion::assert_text_in_region(frame, "harness/gpt-6.1-sol", &full);
                })
            },
        )
        .await?;

    let requests = server.received_requests().await.unwrap_or_default();
    let tool_result_request = requests
        .iter()
        .find(|request| {
            String::from_utf8_lossy(&request.body).contains(r#""type":"function_call_output""#)
        })
        .ok_or("the write result should reach the OpenAI server")?;
    let body = String::from_utf8_lossy(&tool_result_request.body);
    assert_eq!(
        tool_result_request
            .headers
            .get("authorization")
            .map(wiremock::http::HeaderValue::as_bytes),
        Some(b"Bearer test-openai-key".as_slice())
    );
    assert!(body.contains(r#""model":"gpt-6.1-sol""#));
    assert!(body.contains(r#""encrypted_content":"gAAAA-e2e""#));
    assert!(body.contains(r#""store":false"#));

    Ok(())
}

/// Serves Ollama's native listing beside the scripted chat endpoint:
/// tool-calling `qwen3:latest`, loaded with a 32,768-token window, and
/// `gemma:latest`, which cannot call tools.
async fn local_ollama() -> MockServer {
    let server = scripted_provider().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [
            {
                "name": "qwen3:latest",
                "details": {"context_length": 40960},
                "capabilities": ["completion", "tools"]
            },
            {
                "name": "gemma:latest",
                "details": {"context_length": 8192},
                "capabilities": ["completion"]
            }
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{"name": "qwen3:latest", "context_length": 32768}]
        })))
        .mount(&server)
        .await;

    server
}

/// Opens the in-chat `/model` picker of a Harness session.
fn open_model_picker() -> Journey {
    Journey::new("open_harness_model_picker")
        .with_description("Type /model and open the harness model list")
        .step(Step::press_key("/"))
        .step(Step::write_text("model"))
        .step(Step::wait_for_text("Slash Command", 3000))
        .step(Step::press_key("Enter"))
        .step(Step::wait_for_text("/model Harness model", 3000))
}

/// Asserts the picker labels local rows free, paid rows with their price
/// class, and ChatGPT-plan rows `plan` with the sign-in they need, and that
/// `Enter` on a model without tool calls keeps the picker open.
fn assert_picker_captures(report: &ProofReport, full: Region) {
    // Rows pad labels into a column, so compare words.
    let picker_text = common::frame_from_capture(&report.captures[0])
        .all_text()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for row in [
        "[local] qwen3:latest free · Ollama · runs 32k ctx",
        "[local] gemma:latest free · Ollama · no tool calls",
        "[paid] muse-spark-1.3 standard · Muse",
        "[paid] muse-spark-1.3-contributor cheap · Muse · trains on prompts",
        "[paid] kimi-k3 premium · Kimi · needs KIMI_API_KEY",
        "[paid] qwen3.8-flash cheap · Qwen · needs DASHSCOPE_API_KEY",
        "[paid] chatgpt/gpt-6.1-sol plan · ChatGPT plan · needs",
    ] {
        assert!(picker_text.contains(row), "{row} in {picker_text}");
    }
    let gemma = common::frame_from_capture(&report.captures[1]);
    assertion::assert_text_in_region(&gemma, "/model Harness model", &full);
}

#[tokio::test]
async fn test_harness_model_picker_switches_between_local_and_paid_models() -> E2eResult {
    // Arrange
    let ollama = local_ollama().await;
    let paid = MockServer::start().await;
    let git_directory = git_only_path()?;

    // Act, Assert
    FeatureTest::new("harness_model_picker")
        .with_git()
        .with_stub_only_path()
        .env("PATH", git_directory.path().to_string_lossy())
        .args(["--experimental-harness".to_string()])
        .env("DASHSCOPE_API_KEY", "")
        .env("CHATGPT_AUTH_FILE", "")
        .env("KIMI_API_KEY", "")
        .env("OPENAI_API_KEY", "")
        .env("MODEL_API_KEY", "test-key")
        .env("MODEL_API_BASE_URL", paid.uri())
        .env("OPENAI_COMPATIBLE_BASE_URL", ollama.uri())
        .env("OPENAI_COMPATIBLE_MODEL", "")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Install an agent CLI", 5000)
                    .press_key("Enter")
                    // A paid key is set, yet new sessions start on the free
                    // local model.
                    .wait_for_text("harness/openai-compatible/qwen3:latest", 5000)
                    .compose(&open_model_picker())
                    .wait_for_text("chatgpt/gpt-6.1-sol", 3000)
                    .capture_labeled("harness_model_picker", "Local and paid harness models")
                    // Rows: qwen3, gemma, muse-spark-1.3, kimi-k3, ...
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 3000)
                    .capture_labeled("gemma_not_selectable", "A model without tools stays")
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("harness/muse-spark-1.3", 3000)
                    .compose(&open_model_picker())
                    .press_key("Enter")
                    .wait_for_text("harness/openai-compatible/qwen3:latest", 3000)
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
            |frame, report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(
                        frame,
                        "harness/openai-compatible/qwen3:latest",
                        &full,
                    );
                    assert_picker_captures(report, full);
                })
            },
        )
        .await?;

    let chat_requests = ollama
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| request.url.path() == "/chat/completions")
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect::<Vec<_>>();
    assert!(
        chat_requests
            .iter()
            .any(|body| body.contains("Create notes"))
    );
    assert!(
        chat_requests
            .iter()
            .all(|body| body.contains(r#""model":"qwen3:latest""#))
    );
    assert!(
        paid.received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "the paid provider must never be called"
    );

    Ok(())
}

#[tokio::test]
async fn test_harness_model_picker_marks_kimi_ready_with_only_an_api_key() -> E2eResult {
    // Arrange
    let ollama = local_ollama().await;
    let git_directory = git_only_path()?;

    // Act, Assert
    FeatureTest::new("harness_kimi_default_endpoint")
        .with_git()
        .with_stub_only_path()
        .env("PATH", git_directory.path().to_string_lossy())
        .args(["--experimental-harness".to_string()])
        .env("DASHSCOPE_API_KEY", "")
        .env("CHATGPT_AUTH_FILE", "")
        // Kimi has a default endpoint, so a blank base URL needs nothing.
        .env("KIMI_API_KEY", "test-key")
        .env("KIMI_BASE_URL", "")
        .env("OPENAI_API_KEY", "")
        .env("MODEL_API_KEY", "")
        .env("OPENAI_COMPATIBLE_BASE_URL", ollama.uri())
        .env("OPENAI_COMPATIBLE_MODEL", "")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Install an agent CLI", 5000)
                    .press_key("Enter")
                    .wait_for_text("harness/openai-compatible/qwen3:latest", 5000)
                    .compose(&open_model_picker())
                    .wait_for_text("kimi-k3", 3000)
                    .wait_for_stable_frame(300, 3000)
                    .capture_labeled("kimi_ready", "Kimi ready with only its API key")
            },
            |_frame, report| {
                Box::pin(async move {
                    let picker_text = common::frame_from_capture(&report.captures[0])
                        .all_text()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    assert!(
                        picker_text.contains("[paid] kimi-k3 premium · Kimi"),
                        "{picker_text}"
                    );
                    assert!(!picker_text.contains("KIMI_BASE_URL"), "{picker_text}");
                    assert!(!picker_text.contains("needs KIMI_API_KEY"), "{picker_text}");
                })
            },
        )
        .await?;

    Ok(())
}

/// Commit message the cheap Qwen model writes.
const CHEAP_COMMIT_TITLE: &str = "Add notes with a cheap model";

/// Scripts a Qwen endpoint answering utility prompts: the commit message,
/// and any other utility, such as the session title, with a short answer.
async fn cheap_qwen_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            "Generate the canonical session commit message",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": json!({"answer": CHEAP_COMMIT_TITLE}).to_string()}
            }]
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(answer("Harness notes"))
        .with_priority(2)
        .mount(&server)
        .await;

    server
}

/// Opens the Default Fast Model selector on the Settings tab.
fn open_fast_model_selector() -> Journey {
    let mut journey = common::move_to_settings_row(SettingsRow::DefaultFastModel);
    journey.name = "open_fast_model_selector".to_string();

    journey
        .with_description("Open the Default Fast Model selector in Settings")
        .step(Step::press_key("Enter"))
        .step(Step::wait_for_text("Select model", 3000))
}

#[tokio::test]
async fn test_harness_fast_model_writes_commit_messages_on_a_cheap_model() -> E2eResult {
    // Arrange
    let muse = scripted_provider().await;
    let qwen = cheap_qwen_provider().await;
    let git_directory = git_only_path()?;

    // Act, Assert
    FeatureTest::new("harness_cheap_fast_model")
        .with_git()
        .with_stub_only_path()
        .env("PATH", git_directory.path().to_string_lossy())
        .args(["--experimental-harness".to_string()])
        .env("DASHSCOPE_API_KEY", "test-key")
        .env("DASHSCOPE_BASE_URL", qwen.uri())
        .env("KIMI_API_KEY", "")
        .env("OPENAI_API_KEY", "")
        .env("MODEL_API_KEY", "test-key")
        .env("MODEL_API_BASE_URL", muse.uri())
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .compose(&open_fast_model_selector())
                    .wait_for_text("harness/qwen3.8-flash · cheap", 3000)
                    .capture_labeled("fast_model_choices", "Ready harness models by price")
                    // Choices: muse-spark-1.3, its contributor tier,
                    // qwen3.8-max, qwen-plus, then qwen3.8-flash.
                    .press_key("j")
                    .press_key("j")
                    .press_key("j")
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Select reasoning level", 3000)
                    .press_key("Enter")
                    .wait_for_text("harness/qwen3.8-flash [", 3000)
                    .compose(&common::switch_to_tab_reverse("Sessions"))
                    .press_key("a")
                    .wait_for_text("Install an agent CLI", 5000)
                    .press_key("Enter")
                    .wait_for_text("harness/muse-spark-1.3", 5000)
                    .write_text("Create notes")
                    .wait_for_text("Create notes", 3000)
                    .press_key("Enter")
                    // The cheap model's commit message titles the session.
                    .eventually(
                        Duration::from_secs(30),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());

                            assertion::match_text_in_region(frame, CHEAP_COMMIT_TITLE, &full)
                        },
                    )
            },
            |frame, report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "harness/muse-spark-1.3", &full);
                    let choices = common::frame_from_capture(&report.captures[0]).all_text();
                    for label in [
                        "harness/muse-spark-1.3 · standard",
                        // The dropdown truncates this longer label.
                        "harness/muse-spark-1.3-contributor",
                        "harness/qwen-plus · cheap",
                        "harness/qwen3.8-flash · cheap",
                        "harness/qwen3.7-flash · cheap",
                    ] {
                        assert!(choices.contains(label), "{label} in {choices}");
                    }
                })
            },
        )
        .await?;

    let bodies = |requests: Vec<wiremock::Request>| {
        requests
            .into_iter()
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect::<Vec<_>>()
    };
    let qwen_bodies = bodies(qwen.received_requests().await.unwrap_or_default());
    let muse_bodies = bodies(muse.received_requests().await.unwrap_or_default());
    let commit_prompt = "Generate the canonical session commit message";
    // The session turn ran on Muse, and its commit message on the cheap
    // Qwen model.
    assert!(muse_bodies.iter().any(|body| body.contains("Create notes")));
    assert!(!muse_bodies.iter().any(|body| body.contains(commit_prompt)));
    let commit_request = qwen_bodies
        .iter()
        .find(|body| body.contains(commit_prompt))
        .ok_or("the commit message should run on the cheap model")?;
    assert!(commit_request.contains(r#""model":"qwen3.8-flash""#));

    Ok(())
}

/// `ChatGPT` authorization server whose token endpoint grants a plan
/// credential for `user@example.com`.
async fn chatgpt_token_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access",
            "expires_in": 3600,
            "id_token": "header.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20ifQ.signature",
            "refresh_token": "refresh",
            "scope": "openid chatgpt.tokens.use.direct",
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    server
}

/// Installs `open` and `xdg-open` stand-ins into `directory` that approve
/// the sign-in like a user: they request the loopback callback with the
/// URL's `state`, using only Bash built-ins because `PATH` holds no tools.
fn install_approving_browser(directory: &std::path::Path) -> std::io::Result<()> {
    let script = concat!(
        "#!/bin/bash\n",
        "url=\"$1\"\n",
        "state=\"${url#*&state=}\"; state=\"${state%%&*}\"\n",
        "port=\"${url#*127.0.0.1%3A}\"; port=\"${port%%%2F*}\"\n",
        "exec 3<>\"/dev/tcp/127.0.0.1/$port\" || exit 1\n",
        "printf 'GET /auth/callback?state=%s&code=c&client_id=oaiapp_test HTTP/1.1\\r\\n",
        "host: 127.0.0.1\\r\\n\\r\\n' \"$state\" >&3\n",
        "while read -r -u 3 line; do :; done\n",
    );
    for name in ["open", "xdg-open"] {
        let browser = directory.join(name);
        std::fs::write(&browser, script)?;
        std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o750))?;
    }

    Ok(())
}

#[tokio::test]
async fn test_harness_model_picker_signs_in_with_chatgpt() -> E2eResult {
    // Arrange
    let auth_server = chatgpt_token_server().await;
    let paid = MockServer::start().await;
    let git_directory = git_only_path()?;
    install_approving_browser(git_directory.path())?;
    let auth_directory = tempfile::tempdir()?;
    let auth_file = auth_directory.path().join("chatgpt.json");

    // Act, Assert
    FeatureTest::new("harness_chatgpt_sign_in")
        .with_git()
        .with_stub_only_path()
        .env("PATH", git_directory.path().to_string_lossy())
        .args(["--experimental-harness".to_string()])
        .env("CHATGPT_AUTH_FILE", auth_file.to_string_lossy())
        .env("CHATGPT_AUTH_URL", auth_server.uri())
        .env("DASHSCOPE_API_KEY", "")
        .env("KIMI_API_KEY", "")
        .env("OPENAI_API_KEY", "")
        .env("MODEL_API_KEY", "test-key")
        .env("MODEL_API_BASE_URL", paid.uri())
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
        .env("OPENAI_COMPATIBLE_MODEL", "")
        .run(
            |scenario| {
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Install an agent CLI", 5000)
                    .press_key("Enter")
                    .wait_for_text("harness/muse-spark-1.3", 5000)
                    .compose(&open_model_picker())
                    .wait_for_text("chatgpt/gpt-6.1-sol", 3000);
                // Rows: the no-local-models hint, two Muse, two Kimi, and
                // four Qwen models precede `chatgpt/gpt-6.1-sol`.
                (0..9)
                    .fold(scenario, |scenario, _| scenario.press_key("j"))
                    .press_key("Enter")
                    .wait_for_text("Sign in with ChatGPT", 3000)
                    .capture_labeled("chatgpt_sign_in", "ChatGPT sign-in popup")
                    .press_key("Enter")
                    .wait_for_text("Signed in as user@example.com", 15000)
                    .capture_labeled("chatgpt_signed_in", "ChatGPT sign-in succeeded")
                    .press_key("Enter")
                    .wait_for_text("harness/chatgpt/gpt-6.1-sol", 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "harness/chatgpt/gpt-6.1-sol", &full);
                    let popup = common::frame_from_capture(&report.captures[0]).all_text();
                    assert!(popup.contains("Use your ChatGPT plan for"), "{popup}");
                    assert!(popup.contains("Enter: Sign in"), "{popup}");
                })
            },
        )
        .await?;

    assert!(
        auth_file.is_file(),
        "the sign-in should store the credential"
    );
    assert!(
        paid.received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "the paid provider must never be called"
    );

    Ok(())
}

/// Prompt of the streamed-answer scenario.
const STREAM_PROMPT: &str = "Stream a reply";
/// Reasoning the gated provider streams before its answer.
const STREAM_REASONING: &str = "Planning the reply.";
/// Answer text the gated provider streams before its gate opens.
const STREAM_ANSWER_HEAD: &str = "Streaming part one";
/// Answer text the gated provider streams after its gate opens.
const STREAM_ANSWER_TAIL: &str = " and part two.";
/// Heading of the title-generation prompt, which repeats the session request.
const TITLE_PROMPT: &str = "Generate a concise, commit-style title";

/// Serves Chat Completions on a local port and returns its base URL.
///
/// The session turn streams its reasoning and the first part of its answer,
/// then holds the response open until `gate` is released before streaming
/// the rest. Every other request, such as title generation, gets
/// `404 Not Found`.
fn gated_stream_provider(gate: ReleaseGate) -> std::io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let gate = gate.clone();
            std::thread::spawn(move || serve_gated_stream(stream, &gate));
        }
    });

    Ok(format!("http://{address}"))
}

/// Answers one provider connection for [`gated_stream_provider`].
fn serve_gated_stream(mut stream: TcpStream, gate: &ReleaseGate) {
    let Some(body) = read_request_body(&mut stream) else {
        return;
    };
    if !body.contains(STREAM_PROMPT) || body.contains(TITLE_PROMPT) {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");

        return;
    }
    let answer = json!({
        "answer": format!("{STREAM_ANSWER_HEAD}{STREAM_ANSWER_TAIL}"),
        "questions": []
    })
    .to_string();
    let (head, tail) = answer.split_at(answer.find(STREAM_ANSWER_TAIL).unwrap_or_default());
    let finished = json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 30, "completion_tokens": 7, "total_tokens": 37}
    });
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    );
    let _ = stream.write_all(
        sse_chunks(&[
            json!({"choices": [{"index": 0, "delta": {"reasoning_content": STREAM_REASONING}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": head}}]}),
        ])
        .as_bytes(),
    );
    gate.wait_blocking(Duration::from_secs(60));
    let _ = stream.write_all(
        sse_chunks(&[
            json!({"choices": [{"index": 0, "delta": {"content": tail}}]}),
            finished,
        ])
        .as_bytes(),
    );
    let _ = stream.write_all(b"data: [DONE]\n\n");
}

/// Formats `chunks` as server-sent events.
fn sse_chunks(chunks: &[serde_json::Value]) -> String {
    chunks.iter().fold(String::new(), |mut events, chunk| {
        let _ = write!(events, "data: {chunk}\n\n");

        events
    })
}

/// Reads one HTTP request from `stream` and returns its body, or `None`
/// when the connection closes first.
fn read_request_body(stream: &mut TcpStream) -> Option<String> {
    let mut request = Vec::new();
    let mut chunk = [0; 8192];
    let header_end = loop {
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        let read = stream.read(&mut chunk).ok().filter(|read| *read > 0)?;
        request.extend_from_slice(&chunk[..read]);
    };
    let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|length| length.trim().parse::<usize>().ok())
        .unwrap_or_default();
    while request.len() < header_end + length {
        let read = stream.read(&mut chunk).ok().filter(|read| *read > 0)?;
        request.extend_from_slice(&chunk[..read]);
    }

    Some(String::from_utf8_lossy(&request[header_end..]).into_owned())
}

#[tokio::test]
async fn test_harness_session_streams_its_answer_before_the_turn_completes() -> E2eResult {
    // Arrange
    let gate = ReleaseGate::new()?;
    let server = gated_stream_provider(gate.clone())?;

    // Act, Assert
    FeatureTest::new("harness_streaming")
        .with_git()
        .args(["--experimental-harness".to_string()])
        .env("OPENAI_COMPATIBLE_BASE_URL", NO_LOCAL_SERVER)
        .env("MODEL_API_KEY", "test-key")
        .env("MODEL_API_BASE_URL", server)
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
                    .press_key("Enter")
                    .step(common::wait_for_prompt_composer())
                    .write_text(STREAM_PROMPT)
                    .wait_for_text(STREAM_PROMPT, 3000)
                    .press_key("Enter")
                    // The provider holds the rest of its answer until the
                    // gate opens, so this frame is mid-stream.
                    .eventually(
                        Duration::from_secs(30),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());
                            assertion::match_text_in_region(frame, STREAM_REASONING, &full)?;
                            assertion::match_text_in_region(frame, STREAM_ANSWER_HEAD, &full)?;

                            assertion::match_not_visible(frame, STREAM_ANSWER_TAIL.trim())
                        },
                    )
                    .capture_labeled(
                        "harness_streaming_partial",
                        "The answer streams before the turn completes",
                    )
                    .compose(&gate.release())
                    // The persisted answer replaces the stream, whose
                    // reasoning is never stored.
                    .eventually(
                        Duration::from_secs(30),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());
                            assertion::match_text_in_region(
                                frame,
                                &format!("{STREAM_ANSWER_HEAD}{STREAM_ANSWER_TAIL}"),
                                &full,
                            )?;

                            assertion::match_not_visible(frame, STREAM_REASONING)
                        },
                    )
                    .wait_for_stable_frame(500, 10000)
                    .capture_labeled(
                        "harness_streaming_answer",
                        "The persisted answer replaces the stream",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    assertion::assert_match_count(frame, STREAM_ANSWER_TAIL.trim(), 1);
                    assertion::assert_not_visible(frame, STREAM_REASONING);
                })
            },
        )
        .await
}
