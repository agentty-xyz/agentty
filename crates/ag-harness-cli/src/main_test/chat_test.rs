//! Chat loop unit tests for interactive, non-interactive, and one-shot turns,
//! failure reporting, slash commands, and model switching.

use std::env;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use ag_harness::model::ModelProfile;
use ag_harness::provider::ModelProvider;
use ag_harness::{ErrorKind, Harness, ModelError, Repository, ToolSet, TurnError};
use serde_json::json;
use tokio::io::BufReader;

use super::support::{FailOnceModel, FixedModel, test_catalog, test_git_executable};
use crate::{
    ChatMode, CliError, MODEL_PROFILE, ModelSelection, ModelSwitcher, chat_schema, run_chat,
};

/// Switcher over [`test_catalog`] starting on the uncataloged
/// `muse/test-model`, whose connected clients answer with the selected
/// model identifier.
fn test_models()
-> ModelSwitcher<impl FnMut(&ModelSelection) -> Result<(FixedModel, ModelProfile), CliError>> {
    ModelSwitcher {
        catalog: Arc::new(test_catalog()),
        connect: |selection: &ModelSelection| {
            Ok((
                FixedModel(json!({"message": format!("from {}", selection.model)})),
                MODEL_PROFILE,
            ))
        },
        selection: ModelSelection {
            model: "test-model".to_string(),
            provider: ModelProvider::Muse,
        },
    }
}

#[tokio::test]
async fn interactive_chat_prints_prompts_and_handles_blank_input() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let repository = Repository::new(env!("CARGO_MANIFEST_DIR"), test_git_executable())
        .expect("repository fixture should be valid");
    let harness = Harness::new(
        FixedModel(json!({"message": "hello"})),
        MODEL_PROFILE.context_budget().expect("CLI context budget"),
    )
    .database(directory.path().join("harness.db"))
    .repository(repository)
    .tools(ToolSet::empty().with_read());
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
        test_models(),
        None,
        input,
        &mut output,
        ChatMode::Interactive,
        None,
    )
    .await
    .expect("interactive chat should finish at EOF");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.starts_with(
        "Chat with test-model. Type / for commands, Ctrl-D to exit.\n>>> >>> assistant> hello\n"
    ));
    assert!(output.contains("output; test-model; unavailable;"));
    assert!(output.contains("tokens unavailable"));
    assert!(output.ends_with("tools: none\n>>> "));
}

#[tokio::test]
async fn interactive_chat_continues_after_a_failed_turn() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(
        FailOnceModel {
            requests: AtomicUsize::new(0),
        },
        MODEL_PROFILE.context_budget().expect("CLI context budget"),
    )
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
        test_models(),
        None,
        input,
        &mut output,
        ChatMode::Interactive,
        None,
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
    let harness = Harness::new(
        FailOnceModel {
            requests: AtomicUsize::new(0),
        },
        MODEL_PROFILE.context_budget().expect("CLI context budget"),
    )
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
        test_models(),
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
        None,
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
    let harness = Harness::new(
        FailOnceModel {
            requests: AtomicUsize::new(0),
        },
        MODEL_PROFILE.context_budget().expect("CLI context budget"),
    )
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
        test_models(),
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
        None,
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
    let harness = Harness::new(
        FixedModel(json!({"unexpected": true})),
        MODEL_PROFILE.context_budget().expect("CLI context budget"),
    )
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
        test_models(),
        Some("question".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
        None,
    )
    .await
    .expect_err("schema-invalid output should fail");

    // Assert
    assert!(matches!(
        &error,
        CliError::Run(error) if error.kind() == ErrorKind::InvalidOutput
            && matches!(
                error.get_ref().downcast_ref::<ag_harness::SessionError>(),
                Some(ag_harness::SessionError::Turn(TurnError::Model(
                    ModelError::SchemaViolation { path, .. }
                ))) if path == "$"
            )
    ));
    assert_eq!(output, [] as [u8; 0]);
}

#[tokio::test]
async fn one_shot_chat_does_not_read_follow_up_terminal_input() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(
        FixedModel(json!({"message": "hello"})),
        MODEL_PROFILE.context_budget().expect("CLI context budget"),
    )
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
        test_models(),
        Some("question".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
        None,
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
    let harness = Harness::new(
        FailOnceModel {
            requests: AtomicUsize::new(0),
        },
        MODEL_PROFILE.context_budget().expect("CLI context budget"),
    )
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
        test_models(),
        Some("question".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
        None,
    )
    .await
    .expect_err("one-shot chat should return its failed turn");

    // Assert
    assert!(matches!(error, CliError::Run(_)));
    assert_eq!(output, [] as [u8; 0]);
}

#[tokio::test]
async fn interactive_chat_lists_commands_models_and_reports_unknown_commands() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut models = test_models();
    let harness = models
        .harness(true)
        .expect("registered harness should build")
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", chat_schema().expect("schema"))
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"/\n/model\n/bogus now\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        models,
        None,
        input,
        &mut output,
        ChatMode::Interactive,
        None,
    )
    .await
    .expect("interactive commands should not fail the chat");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.contains(">>> commands:\n  /model            list models\n"));
    assert!(output.contains(">>> current model: muse/test-model\n  1. host/model\n"));
    assert!(output.contains("  2. kimi/kimi-listed (cheap) $0.60 in, $2.50 out per 1M tokens\n"));
    assert!(output.contains("Type /model <MODEL> to switch.\n"));
    assert!(output.contains("error: unknown command `/bogus`; type /help for commands\n"));
    assert!(!output.contains("assistant>"));
}

#[tokio::test]
async fn chat_switches_registered_sessions_to_the_selected_model() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut models = test_models();
    let harness = models
        .harness(true)
        .expect("registered harness should build")
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", chat_schema().expect("schema"))
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"first\n/model kimi/kimi-test\nsecond\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        models,
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
        None,
    )
    .await
    .expect("switched chat should succeed");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.starts_with("assistant> from test-model\n"));
    assert!(output.contains("model: kimi/kimi-test\nassistant> from kimi-test\n"));
    assert!(output.contains("output; kimi-test; unavailable;"));
}

#[tokio::test]
async fn chat_switches_direct_sessions_by_catalog_number() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut models = test_models();
    let harness = models
        .harness(false)
        .expect("direct harness should build")
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", chat_schema().expect("schema"))
        .create()
        .await
        .expect("session should be created");
    let input =
        BufReader::new(&b"/model 2\nquestion\n/model cheap\n/model qwen/qwen-listed\nlast\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        models,
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
        None,
    )
    .await
    .expect("direct session should switch");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.starts_with("model: kimi/kimi-listed\nassistant> from kimi-listed\n"));
    assert!(output.contains("model: kimi/kimi-listed\nmodel: qwen/qwen-listed\n"));
    assert!(output.contains("assistant> from qwen-listed\n"));
    // A registered model has no prices, so switched runs report no cost,
    // even on the priced `kimi/kimi-listed`.
    assert_eq!(output.matches("; cost unavailable\n").count(), 2);
    assert!(!output.contains("cost $"));
}

#[tokio::test]
async fn cataloged_models_report_priced_usage_and_session_history() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut models = test_models();
    models.selection = ModelSelection {
        model: "kimi-listed".to_string(),
        provider: ModelProvider::Kimi,
    };
    let harness = models
        .harness(false)
        .expect("cataloged harness should build")
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", chat_schema().expect("schema"))
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"/history\n/usage\nfirst\nsecond\n/history\n/usage\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        models,
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
        None,
    )
    .await
    .expect("priced chat should succeed");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.starts_with(
        "no turns yet\nsession usage: 0 turns; tokens 0 in, 0 out; cost unavailable\n"
    ));
    assert!(output.contains(
        "\nusage: tokens 0 in, 0 out, 1 model calls unreported; cost $0.00\nmodel calls: 1\n"
    ));
    assert!(output.contains(
        "turn 1 (completed)\nyou> first\nassistant> from kimi-listed\nturn 2 (completed)\nyou> \
         second\nassistant> from kimi-listed\n"
    ));
    assert!(output.ends_with(
        "session usage: 2 turns; tokens 0 in, 0 out, 2 model calls unreported; cost $0.00\n"
    ));
}

#[tokio::test]
async fn cataloged_models_without_a_profile_assume_the_cli_profile() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut models = test_models();
    models.selection = ModelSelection {
        model: "qwen-listed".to_string(),
        provider: ModelProvider::Qwen,
    };
    let harness = models
        .harness(false)
        .expect("a cataloged model without a profile should build")
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", chat_schema().expect("schema"))
        .create()
        .await
        .expect("session should be created");
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        models,
        Some("question".to_string()),
        BufReader::new(&b""[..]),
        &mut output,
        ChatMode::OneShot,
        None,
    )
    .await
    .expect("unpriced cataloged chat should succeed");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.starts_with("assistant> from qwen-listed\n"));
    assert!(output.contains("; cost unavailable\n"));
}

#[tokio::test]
async fn noninteractive_chat_reports_failed_switches_and_keeps_the_model() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut models = test_models();
    let harness = models
        .harness(true)
        .expect("registered harness should build")
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", chat_schema().expect("schema"))
        .create()
        .await
        .expect("session should be created");
    let long_model = "m".repeat(300);
    let input = format!("/model 999\n/model nope/model\n/model {long_model}\nquestion\n");
    let input = BufReader::new(input.as_bytes());
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        models,
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
        None,
    )
    .await
    .expect_err("failed switches should fail the noninteractive chat");

    // Assert
    assert!(matches!(error, CliError::ChatTurnsFailed));
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.starts_with(&format!(
        "error: unknown model `999`; type /model to list models\nerror: unknown model \
         `nope/model`; type /model to list models\nerror: cannot switch to `muse/{long_model}`; \
         `provider/model` must fit in 256 bytes\nassistant> from test-model\n"
    )));
}

#[tokio::test]
async fn model_ids_beyond_the_registration_limit_use_a_direct_harness() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut models = test_models();
    models.selection.model = "m".repeat(300);

    // Act
    let harness = models
        .harness(true)
        .expect("oversized model IDs should still build a harness");

    // Assert
    assert!(harness.model_registration().is_none());
    harness
        .database(directory.path().join("harness.db"))
        .session("session-a", chat_schema().expect("schema"))
        .create()
        .await
        .expect("an unregistered session should be created");
}

#[tokio::test]
async fn one_shot_chat_returns_command_failures() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let mut models = test_models();
    let harness = models
        .harness(true)
        .expect("registered harness should build")
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", chat_schema().expect("schema"))
        .create()
        .await
        .expect("session should be created");
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        models,
        Some("/unknown".to_string()),
        BufReader::new(&b""[..]),
        &mut output,
        ChatMode::OneShot,
        None,
    )
    .await
    .expect_err("one-shot command failures should be returned");

    // Assert
    assert!(matches!(error, CliError::UnknownCommand { name } if name == "/unknown"));
    assert_eq!(output, [] as [u8; 0]);
}
