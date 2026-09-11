use std::env;
use std::sync::atomic::AtomicUsize;

use ag_harness::{Harness, Repository, Tool};
use serde_json::json;
use tokio::io::BufReader;

use super::support::{FailOnceModel, FixedModel, test_git_executable};
use crate::{ChatMode, CliError, chat_schema, run_chat};

#[tokio::test]
async fn interactive_chat_prints_prompts_and_handles_blank_input() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let repository = Repository::new(env!("CARGO_MANIFEST_DIR"), test_git_executable())
        .expect("repository fixture should be valid");
    let harness = Harness::new(FixedModel(json!({"message": "hello"})))
        .database(directory.path().join("harness.db"))
        .repository(repository)
        .allow(Tool::Read);
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"\nquestion\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        "test-model",
        None,
        input,
        &mut output,
        ChatMode::Interactive,
    )
    .await
    .expect("interactive chat should finish at EOF");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(
        output.starts_with("Chat with test-model. Ctrl-D to exit.\n>>> >>> assistant> hello\n")
    );
    assert!(output.contains("output; test-model; unavailable;"));
    assert!(output.contains("tokens unavailable"));
    assert!(output.ends_with("tools: none\n>>> "));
}

#[tokio::test]
async fn interactive_chat_continues_after_a_failed_turn() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FailOnceModel {
        requests: AtomicUsize::new(0),
    })
    .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"first\nretry\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        "test-model",
        None,
        input,
        &mut output,
        ChatMode::Interactive,
    )
    .await
    .expect("interactive chat should recover and finish at EOF");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.contains("error: model returned no response content\n"));
    assert!(output.contains(">>> assistant> recovered\n---\n"));
    assert!(output.ends_with("tools: none\n>>> "));
}

#[tokio::test]
async fn noninteractive_chat_reports_a_failure_before_retrying() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FailOnceModel {
        requests: AtomicUsize::new(0),
    })
    .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"first\nretry\n"[..]);
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        "test-model",
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
    )
    .await
    .expect_err("a recovered chat should retain its failed exit status");

    // Assert
    assert!(matches!(error, CliError::ChatTurnsFailed));
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.starts_with("error: model returned no response content\n"));
    assert!(output.contains("assistant> recovered\n---\n"));
}

#[tokio::test]
async fn noninteractive_chat_returns_the_last_failure_at_eof() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FailOnceModel {
        requests: AtomicUsize::new(0),
    })
    .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"first\n"[..]);
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        "test-model",
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
    )
    .await
    .expect_err("the final failed turn should be returned at EOF");

    // Assert
    assert!(matches!(error, CliError::ChatTurnsFailed));
    assert_eq!(
        String::from_utf8(output).expect("chat output should be UTF-8"),
        "error: model returned no response content\n"
    );
}

#[tokio::test]
async fn chat_rejects_model_output_that_violates_schema() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FixedModel(json!({"unexpected": true})))
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        "test-model",
        Some("question".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect_err("schema-invalid output should fail");

    // Assert
    assert!(matches!(
        error,
        CliError::Session(ag_harness::SessionError::Turn(
            ag_harness::TurnError::Model(
                ag_harness::ModelError::SchemaViolation { path, .. }
            )
        )) if path == "$"
    ));
    assert_eq!(output, [] as [u8; 0]);
}

#[tokio::test]
async fn one_shot_chat_does_not_read_follow_up_terminal_input() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FixedModel(json!({"message": "hello"})))
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"unexpected follow-up\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        "test-model",
        Some("question".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect("one-shot chat should finish after the initial prompt");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert_eq!(output.matches("assistant> hello\n---\n").count(), 1);
    assert!(!output.contains("Chat with"));
    assert!(!output.contains(">>>"));
}

#[tokio::test]
async fn one_shot_chat_returns_turn_failures() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FailOnceModel {
        requests: AtomicUsize::new(0),
    })
    .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        "test-model",
        Some("question".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect_err("one-shot chat should return its failed turn");

    // Assert
    assert!(matches!(error, CliError::Session(_)));
    assert_eq!(output, [] as [u8; 0]);
}
