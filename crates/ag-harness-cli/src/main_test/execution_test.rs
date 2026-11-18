//! Command execution unit tests for session creation and resume, credential
//! checks, and advertised repository tool permissions.

use std::env;
use std::time::Duration;

use ag_harness::model::ReasoningEffort;
use ag_harness::provider::ModelConfigurationError;
use clap::Parser as _;
use serde_json::json;
use tokio::io::BufReader;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{
    parse_cli, provider_response, test_git_executable, with_repository_controlled_git,
};
use crate::serve::PROTOCOL_SCHEMA;
use crate::{ChatMode, Cli, CliError, Command, READ_WRITE_SYSTEM_PROMPT, execute, serve_settings};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_advertises_repository_reads_by_default() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""name":"read""#))
        .and(body_string_contains(r#""content":"Hello","role":"user""#))
        .respond_with(provider_response("hello"))
        .expect(1)
        .mount(&server)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let database = storage.path().join("harness.db");
    let cli = parse_cli([
        "ag-harness",
        "run",
        "muse-test",
        "Hello",
        "--base-url",
        &server.uri(),
        "--database",
        &database.to_string_lossy(),
    ])
    .expect("chat arguments should parse");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    execute(
        cli,
        |_| Ok("test-key".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect("chat with default repository reads should succeed");

    // Assert
    assert!(
        String::from_utf8(output)
            .expect("chat output should be UTF-8")
            .contains("assistant> hello\n---\n")
    );
}

/// Returns a test key for every variable except the `ChatGPT` sign-in
/// locations, so the catalog skips the `ChatGPT` models a test does not use.
fn api_keys_without_sign_in(name: &str) -> Result<String, env::VarError> {
    match name {
        "AGENTTY_ROOT" | "CHATGPT_AUTH_FILE" | "HOME" => Err(env::VarError::NotPresent),
        _ => Ok("test-key".to_string()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_resumes_a_saved_session_with_its_model_identity_and_history() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""content":"second","role":"user""#))
        .and(body_string_contains(r#""content":"first","role":"user""#))
        .respond_with(provider_response("second answer"))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""content":"first","role":"user""#))
        .respond_with(provider_response("first answer"))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let database = storage.path().join("harness.db");
    let run = parse_cli([
        "ag-harness",
        "--database",
        &database.to_string_lossy(),
        "run",
        "muse-test",
        "first",
        "--session",
        "session-a",
        "--base-url",
        &server.uri(),
    ])
    .expect("run arguments should parse");
    let resume = parse_cli([
        "ag-harness",
        "--database",
        &database.to_string_lossy(),
        "resume",
        "session-a",
        "second",
        "--base-url",
        &server.uri(),
    ])
    .expect("resume arguments should parse");
    let invalid_resume = with_repository_controlled_git(
        parse_cli([
            "ag-harness",
            "--database",
            &database.to_string_lossy(),
            "resume",
            "session-a",
            "third",
            "--base-url",
            &server.uri(),
        ])
        .expect("invalid resume fixture should parse"),
    )
    .expect("repository-controlled Git fixture should resolve");
    let mut first_output = Vec::new();
    let mut second_output = Vec::new();

    // Act
    execute(
        run,
        api_keys_without_sign_in,
        BufReader::new(&b""[..]),
        &mut first_output,
        ChatMode::OneShot,
    )
    .await
    .expect("first process should create the session");
    execute(
        resume,
        api_keys_without_sign_in,
        BufReader::new(&b""[..]),
        &mut second_output,
        ChatMode::OneShot,
    )
    .await
    .expect("second process should resume the session");
    let invalid_error = execute(
        invalid_resume,
        api_keys_without_sign_in,
        BufReader::new(&b""[..]),
        Vec::new(),
        ChatMode::OneShot,
    )
    .await
    .expect_err("repository-controlled Git should reject resume");

    // Assert
    let first_output = String::from_utf8(first_output).expect("output should be UTF-8");
    let second_output = String::from_utf8(second_output).expect("output should be UTF-8");
    assert!(first_output.contains("session: session-a\n"));
    assert!(first_output.contains("assistant> first answer\n"));
    assert!(second_output.contains("session: session-a\n"));
    assert!(second_output.contains("assistant> second answer\n"));
    assert!(matches!(
        invalid_error,
        CliError::Repository(ag_harness::RepositoryError::GitExecutableInsideRepository { .. })
    ));
}

#[tokio::test]
async fn execute_reports_missing_provider_credentials_before_creating_a_session() {
    // Arrange
    let cli = parse_cli([
        "ag-harness",
        "--database",
        "unused.db",
        "run",
        "muse-test",
        "Hello",
    ])
    .expect("run arguments should parse");
    let mut output = Vec::new();

    // Act
    let error = execute(
        cli,
        |_| Err(env::VarError::NotPresent),
        BufReader::new(&b""[..]),
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect_err("missing credentials should fail");

    // Assert
    assert!(matches!(
        error,
        CliError::ModelConfiguration(ModelConfigurationError::ApiKey { .. })
    ));
    assert_eq!(output, [] as [u8; 0]);
}

#[tokio::test]
async fn execute_rejects_repository_controlled_git_for_new_sessions() {
    // Arrange
    let cli = with_repository_controlled_git(
        parse_cli([
            "ag-harness",
            "--database",
            "unused.db",
            "run",
            "muse-test",
            "Hello",
        ])
        .expect("run arguments should parse"),
    )
    .expect("repository-controlled Git fixture should resolve");

    // Act
    let error = execute(
        cli,
        |_| Ok("test-key".to_string()),
        BufReader::new(&b""[..]),
        Vec::new(),
        ChatMode::OneShot,
    )
    .await
    .expect_err("repository-controlled Git should reject a new session");

    // Assert
    assert!(matches!(
        error,
        CliError::Repository(ag_harness::RepositoryError::GitExecutableInsideRepository { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_advertises_writes_only_when_explicitly_enabled() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(READ_WRITE_SYSTEM_PROMPT))
        .and(body_string_contains(r#""name":"read""#))
        .and(body_string_contains(r#""name":"write""#))
        .respond_with(provider_response("ready"))
        .expect(1)
        .mount(&server)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let database = storage.path().join("harness.db");
    let cli = parse_cli([
        "ag-harness",
        "run",
        "muse-test",
        "Hello",
        "--base-url",
        &server.uri(),
        "--allow-write",
        "--database",
        &database.to_string_lossy(),
    ])
    .expect("write-enabled chat arguments should parse");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    execute(
        cli,
        |_| Ok("test-key".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect("write-enabled chat should succeed");

    // Assert
    assert!(
        String::from_utf8(output)
            .expect("chat output should be UTF-8")
            .contains("assistant> ready\n---\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_advertises_read_only_with_an_explicit_directory() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""name":"read""#))
        .and(body_string_contains(r#""content":"Hello","role":"user""#))
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
                            "arguments": r#"{"path":"input.txt"}"#
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
        .respond_with(provider_response("hello"))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let repository = tempfile::TempDir::new().expect("temporary repository should exist");
    let database = repository.path().join("harness.db");
    std::fs::write(repository.path().join("input.txt"), "contents")
        .expect("read fixture should be written");
    let cli = parse_cli([
        "ag-harness",
        "run",
        "muse-test",
        "Hello",
        "--base-url",
        &server.uri(),
        "--read-dir",
        &repository.path().to_string_lossy(),
        "--database",
        &database.to_string_lossy(),
    ])
    .expect("chat arguments should parse");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    execute(
        cli,
        |_| Ok("test-key".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect("chat with explicit read access should succeed");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.contains("assistant> hello\n---\n"));
    assert!(output.contains("tools:\n  read input.txt (lines 1-1;"));
}

#[tokio::test]
async fn execute_serves_the_protocol_and_prints_its_schema() {
    // Arrange
    let serve = Cli::try_parse_from(["ag-harness", "serve"]).expect("serve should parse");
    let print_schema =
        Cli::try_parse_from(["ag-harness", "serve", "--print-schema"]).expect("serve should parse");
    let input = BufReader::new(&b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"shutdown\"}\n"[..]);
    let mut served = Vec::new();
    let mut schema = Vec::new();

    // Act
    let serve_result = execute(
        serve,
        |_| Err(env::VarError::NotPresent),
        input,
        &mut served,
        ChatMode::NonInteractive,
    )
    .await;
    let schema_result = execute(
        print_schema,
        |_| Err(env::VarError::NotPresent),
        BufReader::new(&b""[..]),
        &mut schema,
        ChatMode::NonInteractive,
    )
    .await;

    // Assert
    assert!(serve_result.is_ok() && schema_result.is_ok());
    assert_eq!(
        String::from_utf8(served).expect("serve output should be UTF-8"),
        "{\"id\":1,\"jsonrpc\":\"2.0\",\"result\":{}}\n"
    );
    assert_eq!(
        String::from_utf8(schema).expect("schema should be UTF-8"),
        PROTOCOL_SCHEMA
    );
}

#[test]
fn serve_offers_repository_tools_only_with_a_read_dir() {
    // Arrange
    let repository = tempfile::tempdir().expect("temporary repository should exist");
    let root = repository.path().to_string_lossy().into_owned();
    let git = test_git_executable().to_string_lossy().into_owned();
    let parse = |args: &[&str]| {
        let cli = Cli::try_parse_from(args).expect("serve should parse");
        let Command::Serve(serve) = &cli.command else {
            unreachable!("parsed `serve`");
        };
        let settings = serve_settings(serve, ReasoningEffort::Low, cli.git_executable.clone())
            .expect("settings should resolve");

        (cli.option_serve_ignores(), settings)
    };

    // Act
    let (_, without_tools) = parse(&["ag-harness", "serve"]);
    let (_, read_only) = parse(&["ag-harness", "serve", "--read-dir", &root]);
    let (ignored, writable) = parse(&[
        "ag-harness",
        "--git-executable",
        &git,
        "serve",
        "--read-dir",
        &root,
        "--allow-write",
        "--approval-timeout-ms",
        "250",
    ]);
    let write_without_dir = Cli::try_parse_from(["ag-harness", "serve", "--allow-write"]);

    // Assert
    assert!(without_tools.tools.is_none());
    assert_eq!(without_tools.approval_timeout, Duration::from_secs(300));
    let tool_names = |settings: &crate::serve::Settings| {
        settings
            .tools
            .as_ref()
            .map(|(_, tools)| tools.names().map(str::to_string).collect::<Vec<_>>())
    };
    assert_eq!(tool_names(&read_only), Some(vec!["read".to_string()]));
    assert_eq!(
        tool_names(&writable),
        Some(vec!["read".to_string(), "write".to_string()])
    );
    assert_eq!(writable.approval_timeout, Duration::from_millis(250));
    assert_eq!(ignored, None, "--git-executable applies with --read-dir");
    assert!(write_without_dir.is_err(), "--allow-write needs --read-dir");
}

#[tokio::test]
async fn execute_rejects_global_options_serve_ignores() {
    // Arrange
    let options = [
        ("--comparison-base", "HEAD"),
        ("--database", "harness.db"),
        ("--git-executable", "/usr/bin/git"),
        ("--otlp-endpoint", "http://127.0.0.1:9/v1/traces"),
    ];

    // Act
    let mut rejected = Vec::new();
    for (option, value) in options {
        let cli = Cli::try_parse_from(["ag-harness", option, value, "serve"])
            .expect("serve with a global option should parse");
        let result = execute(
            cli,
            |_| Err(env::VarError::NotPresent),
            BufReader::new(&b""[..]),
            Vec::new(),
            ChatMode::NonInteractive,
        )
        .await;
        rejected.push(result.err().map(|error| error.to_string()));
    }

    // Assert
    for ((option, _), error) in options.iter().zip(rejected) {
        assert!(
            matches!(error, Some(ref message) if message.starts_with(option)),
            "{option} should be rejected: {error:?}"
        );
    }
}
