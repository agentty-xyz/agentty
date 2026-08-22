//! Process-level coverage for the `ag-harness` command-line interface.

use std::io::Write as _;
use std::process::{Command, Stdio};

use assert_cmd::cargo::cargo_bin;
use serde_json::json;
use wiremock::matchers::{bearer_token, body_json, body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn chat_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "message": {"type": "string"}
        },
        "required": ["message"],
        "additionalProperties": false
    })
}

fn read_tool() -> serde_json::Value {
    json!({
        "type": "function",
        "function": {
            "description": "Read a repository-relative file, optionally selecting a line range.",
            "name": "read",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "^(?:[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.\\.[^/\\\\\\u0000]+)(?:/(?:[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.\\.[^/\\\\\\u0000]+))*$"
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": u64::MAX
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": u64::MAX
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }
    })
}

fn response(message: &str, input_tokens: u64, output_tokens: u64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"message": message}).to_string()}
        }],
        "id": "response-test",
        "model": "muse-reported",
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    }))
}

#[test]
fn help_describes_the_chat_interface() {
    // Arrange
    let mut command = Command::new(cargo_bin("ag-harness"));

    // Act
    let output = command.arg("--help").output().expect("CLI help should run");

    // Assert
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help should be UTF-8");
    assert!(stdout.contains("Chats with models through a read-only repository harness"));
    assert!(stdout.contains("Usage: ag-harness <COMMAND>"));
    assert!(stdout.contains("Starts an in-memory chat"));
    assert!(stdout.contains("Set MODEL_API_KEY, then run ag-harness run <MODEL>."));
    assert!(stdout.contains("Press Ctrl-D to exit."));
    assert!(stdout.contains("ag-harness run muse-spark-1.2 \"Summarize Cargo.toml\""));
    assert!(stdout.contains("ag-harness run --help"));
}

#[test]
fn run_help_describes_optional_initial_prompt() {
    // Arrange
    let mut command = Command::new(cargo_bin("ag-harness"));

    // Act
    let output = command
        .args(["run", "--help"])
        .output()
        .expect("run help should execute");

    // Assert
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help should be UTF-8");
    assert!(stdout.contains("Usage: ag-harness run [OPTIONS] <MODEL> [PROMPT]"));
    assert!(stdout.contains("Optional first prompt"));
    assert!(stdout.contains("--base-url <URL>"));
    assert!(stdout.contains("Prompts share in-memory history"));
    assert!(stdout.contains("writes are disabled"));
    assert!(stdout.contains("files may be sent to the configured provider"));
    assert!(stdout.contains("inspected-file metadata"));
    assert!(stdout.contains("MODEL_API_KEY       Required provider API key."));
    assert!(stdout.contains("MODEL_API_BASE_URL  Optional provider endpoint override."));
    assert!(!stdout.contains("--schema"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_prompt_prints_answer_and_model_metadata() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("test-key"))
        .and(body_json(json!({
            "messages": [{"content": "Hello", "role": "user"}],
            "model": "muse-test",
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "ag_harness_output",
                    "schema": chat_schema()
                }
            },
            "tools": [read_tool()]
        })))
        .respond_with(response("Hi there", 9, 3))
        .expect(1)
        .mount(&server)
        .await;
    let mut command = Command::new(cargo_bin("ag-harness"));

    // Act
    let output = command
        .args(["run", "muse-test", "Hello", "--base-url", &server.uri()])
        .env("MODEL_API_KEY", "test-key")
        .output()
        .expect("CLI request should run");

    // Assert
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.starts_with("Hi there\n---\n"));
    assert!(stdout.contains("model calls: 1\n"));
    assert!(stdout.contains("output; muse-reported; stop;"));
    assert!(stdout.contains("tokens 9 in, 3 out, 12 total"));
    assert!(stdout.ends_with("tools: none\n"));
    assert_eq!(output.stderr, [] as [u8; 0]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdin_prompts_share_conversation_history() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            r#""content":"first question","role":"user""#,
        ))
        .respond_with(response("first answer", 4, 2))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            r#""content":"{\"message\":\"first answer\"}","role":"assistant""#,
        ))
        .and(body_string_contains(
            r#""content":"second question","role":"user""#,
        ))
        .respond_with(response("second answer", 10, 2))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let mut child = Command::new(cargo_bin("ag-harness"))
        .args([
            "run",
            "muse-test",
            "first question",
            "--base-url",
            &server.uri(),
        ])
        .env("MODEL_API_KEY", "test-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI chat should start");

    // Act
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(b"second question\n")
        .expect("second prompt should be written");
    let output = child.wait_with_output().expect("CLI chat should finish");

    // Assert
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.contains("first answer\n---\n"));
    assert!(stdout.contains("second answer\n---\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_tool_reports_the_file_without_printing_its_contents() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            r#""content":"Inspect the manifest","role":"user""#,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call-read",
                        "type": "function",
                        "function": {
                            "name": "read",
                            "arguments": r#"{"path":"Cargo.toml","limit":2}"#
                        }
                    }]
                }
            }]
        })))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""tool_call_id":"call-read""#))
        .respond_with(response("It is a Rust workspace.", 20, 5))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let mut command = Command::new(cargo_bin("ag-harness"));

    // Act
    let output = command
        .args([
            "run",
            "muse-test",
            "Inspect the manifest",
            "--base-url",
            &server.uri(),
        ])
        .env("MODEL_API_KEY", "test-key")
        .output()
        .expect("CLI tool round trip should run");

    // Assert
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.contains("model calls: 2\n"));
    assert!(stdout.contains("tools:\n  read Cargo.toml (lines 1-2, truncated;"));
    assert!(!stdout.contains("[workspace]"));
}

#[test]
fn missing_api_key_fails_without_model_output() {
    // Arrange
    let mut command = Command::new(cargo_bin("ag-harness"));

    // Act
    let output = command
        .args(["run", "muse-test"])
        .env_remove("MODEL_API_KEY")
        .output()
        .expect("CLI failure should run");

    // Assert
    assert!(!output.status.success());
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(output.stderr).expect("error should be UTF-8"),
        "MODEL_API_KEY is unavailable: environment variable not found\n"
    );
}
