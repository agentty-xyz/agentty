//! Mid-turn steering through the CLI channel's `stream-json` stdin.

use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{
    AgentChannel as _, AgentError, AgentRequestKind, TurnEvent, TurnRequest, TurnResult,
};
use ag_protocol::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use ag_session::AgentKind;
use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::support::make_turn_request;
use crate::agent::MockAgentBackend;
use crate::channel::cli::CliAgentChannel;

/// Claude stand-in that answers with the second stdin message it reads,
/// then waits for stdin to close before exiting.
fn steerable_claude_command() -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command.arg("-c").arg(
        "IFS= read -r initial; IFS= read -r steer; case \"$steer\" in *'focus on the parser'*) \
         answer='steered' ;; *) answer='not steered' ;; esac; printf \
         '{\"type\":\"result\",\"result\":\"{\\\\\"answer\\\\\":\\\\\"%s\\\\\",\\\\\"questions\\\\\
         \":[]}\"}\\n' \"$answer\"; cat >/dev/null",
    );

    command
}

/// Claude stand-in that reads the steering prompt only after its first turn
/// ended, so it answers the prompt in a follow-up turn of the same run.
fn late_steered_claude_command() -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command.arg("-c").arg(
        "IFS= read -r initial; IFS= read -r steer; printf '%s\\n' \
         '{\"type\":\"result\",\"result\":\"{\\\"answer\\\":\\\"first\\\",\\\"questions\\\":[]}\",\
         \"usage\":{\"input_tokens\":11,\"output_tokens\":7}}' \
         '{\"type\":\"result\",\"result\":\"{\\\"answer\\\":\\\"second\\\",\\\"questions\\\":[]}\"\
         ,\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}'; cat >/dev/null",
    );

    command
}

/// Claude stand-in whose follow-up turn answers a steer in plain text.
fn malformed_follow_up_claude_command() -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command.arg("-c").arg(
        "IFS= read -r initial; IFS= read -r steer; printf '%s\\n' \
         '{\"type\":\"result\",\"result\":\"{\\\"answer\\\":\\\"first\\\",\\\"questions\\\":[]}\"\
         }' '{\"type\":\"result\",\"result\":\"plain follow-up\"}'; cat >/dev/null",
    );

    command
}

/// Claude stand-in for a protocol-repair turn that still answers in plain
/// text.
fn failed_repair_claude_command() -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command
        .arg("-c")
        .arg("cat >/dev/null; printf '%s\\n' '{\"type\":\"result\",\"result\":\"still plain\"}'");

    command
}

/// Claude stand-in for a utility turn that reads stdin to its end.
fn utility_claude_command() -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command
        .arg("-c")
        .arg("cat >/dev/null; printf '%s' '{\"answer\":\"done\"}'");

    command
}

/// Builds a Claude CLI channel whose every turn runs `command`.
fn claude_channel(command: fn() -> std::process::Command) -> CliAgentChannel {
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .returning(move |_| Ok(command()));

    CliAgentChannel::with_backend(Arc::new(backend), AgentKind::Claude)
}

/// Starts one turn on `channel` and waits until its process is running.
async fn start_turn(
    channel: &CliAgentChannel,
    request: TurnRequest,
) -> JoinHandle<Result<TurnResult, AgentError>> {
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let turn = tokio::spawn(channel.run_turn("session".into(), request, events_tx));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(
            events_rx.recv().await,
            Some(TurnEvent::PidUpdate(Some(_))) | None
        ) {}
    })
    .await
    .expect("turn process starts");

    turn
}

/// Awaits one spawned turn with a deadline that proves stdin was closed.
async fn finish_turn(turn: JoinHandle<Result<TurnResult, AgentError>>) -> TurnResult {
    tokio::time::timeout(Duration::from_secs(10), turn)
        .await
        .expect("stdin closes once the turn reports its result")
        .expect("turn task")
        .expect("turn")
}

#[tokio::test]
async fn claude_session_turn_reads_steering_prompt_and_closes_stdin_after_result() {
    // Arrange
    let directory = tempdir().expect("temporary folder");
    let channel = claude_channel(steerable_claude_command);
    let turn = start_turn(&channel, make_turn_request(directory.path().to_path_buf())).await;

    // Act
    let steered = channel
        .steer_turn("session".into(), "focus on the parser".into())
        .await;
    let result = finish_turn(turn).await;
    let after_turn = channel
        .steer_turn("session".into(), "too late".into())
        .await;

    // Assert
    assert!(steered.is_ok());
    assert_eq!(result.assistant_message.to_display_text(), "steered");
    assert!(matches!(after_turn, Err(AgentError::Runtime(_))));
}

#[tokio::test]
async fn claude_session_turn_keeps_every_answer_when_a_steer_gets_a_follow_up_turn() {
    // Arrange
    let directory = tempdir().expect("temporary folder");
    let channel = claude_channel(late_steered_claude_command);
    let turn = start_turn(&channel, make_turn_request(directory.path().to_path_buf())).await;

    // Act
    let steered = channel
        .steer_turn("session".into(), "focus on the parser".into())
        .await;
    let result = finish_turn(turn).await;

    // Assert
    assert!(steered.is_ok());
    assert_eq!(
        result.assistant_message.to_display_text(),
        "first\n\nsecond"
    );
    assert_eq!(result.input_tokens, 16);
    assert_eq!(result.output_tokens, 10);
}

#[tokio::test]
async fn claude_session_turn_keeps_primary_answer_when_a_follow_up_reply_is_malformed() {
    // Arrange
    let directory = tempdir().expect("temporary folder");
    let mut backend = MockAgentBackend::new();
    let mut sequence = mockall::Sequence::new();
    backend
        .expect_build_command()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(malformed_follow_up_claude_command()));
    backend
        .expect_build_command()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(failed_repair_claude_command()));
    let channel = CliAgentChannel::with_backend(Arc::new(backend), AgentKind::Claude);
    let turn = start_turn(&channel, make_turn_request(directory.path().to_path_buf())).await;

    // Act
    let steered = channel
        .steer_turn("session".into(), "focus on the parser".into())
        .await;
    let result = finish_turn(turn).await;

    // Assert
    assert!(steered.is_ok());
    assert_eq!(
        result.assistant_message.to_display_text(),
        "first\n\nplain follow-up"
    );
}

#[tokio::test]
async fn claude_utility_turn_rejects_steering() {
    // Arrange
    let directory = tempdir().expect("temporary folder");
    let channel = claude_channel(utility_claude_command);
    let mut request = make_turn_request(directory.path().to_path_buf());
    request.request_kind = AgentRequestKind::UtilityPrompt;
    let turn = start_turn(&channel, request).await;

    // Act
    let steered = channel
        .steer_turn("session".into(), "focus on the parser".into())
        .await;
    let result = finish_turn(turn).await;

    // Assert
    assert!(matches!(steered, Err(AgentError::Runtime(_))));
    assert_eq!(result.assistant_message.to_display_text(), "done");
}

#[tokio::test]
async fn claude_session_turn_keeps_image_prompts_for_a_later_turn() {
    // Arrange
    let directory = tempdir().expect("temporary folder");
    let channel = claude_channel(steerable_claude_command);
    let turn = start_turn(&channel, make_turn_request(directory.path().to_path_buf())).await;
    let image_prompt = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: directory.path().join("image.png"),
        }],
        text: "Check [Image #1]".to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    };

    // Act
    let image_steered = channel.steer_turn("session".into(), image_prompt).await;
    let text_steered = channel
        .steer_turn("session".into(), "focus on the parser".into())
        .await;
    let result = finish_turn(turn).await;

    // Assert
    assert_eq!(
        image_steered
            .expect_err("image prompt stays queued")
            .to_string(),
        "Claude cannot receive images during a turn"
    );
    assert!(text_steered.is_ok());
    assert_eq!(result.assistant_message.to_display_text(), "steered");
}
