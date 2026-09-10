use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use mockall::Sequence;
use mockall::predicate::eq;

use super::{
    MockTmuxCommandRunner, ProcessTmuxCommandRunner, RealTmuxClient, TmuxCommandOutput,
    has_tmux_environment,
};

#[test]
fn tmux_environment_detects_nonempty_tmux_variable() {
    // Arrange
    let variables = [("TMUX", Some("/tmp/tmux-501/default,123,0"))];

    // Act
    let is_tmux_session = has_tmux_environment(|name| {
        variables
            .iter()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value.map(OsString::from))
    });

    // Assert
    assert!(is_tmux_session);
}

#[test]
fn tmux_environment_rejects_missing_or_empty_tmux_variable() {
    // Arrange, Act
    let missing_tmux_session = has_tmux_environment(|_| None);
    let empty_tmux_session = has_tmux_environment(|_| Some(OsString::new()));

    // Assert
    assert!(!missing_tmux_session);
    assert!(!empty_tmux_session);
}

#[tokio::test]
async fn open_window_for_folder_with_runner_returns_window_id_on_success() {
    // Arrange
    let mut command_runner = MockTmuxCommandRunner::new();
    let session_folder = PathBuf::from("/tmp/agentty-session");

    command_runner
        .expect_open_window()
        .with(eq(session_folder.clone()))
        .times(1)
        .return_once(|_| Box::pin(async { Ok(successful_tmux_output(b"@42\n")) }));

    // Act
    let window_id =
        RealTmuxClient::open_window_for_folder_with_runner(&command_runner, session_folder).await;

    // Assert
    assert_eq!(window_id, Some("@42".to_string()));
}

#[tokio::test]
async fn open_window_for_folder_with_runner_returns_none_when_command_fails() {
    // Arrange
    let mut command_runner = MockTmuxCommandRunner::new();
    let session_folder = PathBuf::from("/tmp/agentty-session");

    command_runner
        .expect_open_window()
        .with(eq(session_folder.clone()))
        .times(1)
        .return_once(|_| Box::pin(async { Err(io::Error::other("tmux unavailable")) }));

    // Act
    let window_id =
        RealTmuxClient::open_window_for_folder_with_runner(&command_runner, session_folder).await;

    // Assert
    assert_eq!(window_id, None);
}

#[tokio::test]
async fn open_window_for_folder_with_runner_returns_none_when_tmux_exits_unsuccessfully() {
    // Arrange
    let mut command_runner = MockTmuxCommandRunner::new();
    let session_folder = PathBuf::from("/tmp/agentty-session");

    command_runner
        .expect_open_window()
        .with(eq(session_folder.clone()))
        .times(1)
        .return_once(|_| Box::pin(async { Ok(failed_tmux_output()) }));

    // Act
    let window_id =
        RealTmuxClient::open_window_for_folder_with_runner(&command_runner, session_folder).await;

    // Assert
    assert_eq!(window_id, None);
}

#[tokio::test]
async fn run_command_in_window_with_runner_sends_enter_after_literal_keys() {
    // Arrange
    let mut command_runner = MockTmuxCommandRunner::new();
    let mut sequence = Sequence::new();

    command_runner
        .expect_send_literal_keys()
        .with(eq("@42".to_string()), eq("git status".to_string()))
        .times(1)
        .in_sequence(&mut sequence)
        .return_once(|_, _| Box::pin(async { Ok(successful_tmux_output(b"")) }));
    command_runner
        .expect_send_enter_key()
        .with(eq("@42".to_string()))
        .times(1)
        .in_sequence(&mut sequence)
        .return_once(|_| Box::pin(async { Ok(successful_tmux_output(b"")) }));

    // Act
    RealTmuxClient::run_command_in_window_with_runner(
        &command_runner,
        "@42".to_string(),
        "git status".to_string(),
    )
    .await;

    // Assert
    // `mockall` verifies the expected literal-send and Enter sequence.
}

#[tokio::test]
async fn run_command_in_window_with_runner_stops_when_literal_send_fails() {
    // Arrange
    let mut command_runner = MockTmuxCommandRunner::new();

    command_runner
        .expect_send_literal_keys()
        .with(eq("@42".to_string()), eq("git status".to_string()))
        .times(1)
        .return_once(|_, _| Box::pin(async { Err(io::Error::other("tmux send-keys failed")) }));
    command_runner.expect_send_enter_key().times(0);

    // Act
    RealTmuxClient::run_command_in_window_with_runner(
        &command_runner,
        "@42".to_string(),
        "git status".to_string(),
    )
    .await;

    // Assert
    // `mockall` verifies that Enter is not sent after a command error.
}

#[tokio::test]
async fn run_command_in_window_with_runner_stops_when_literal_send_exits_unsuccessfully() {
    // Arrange
    let mut command_runner = MockTmuxCommandRunner::new();

    command_runner
        .expect_send_literal_keys()
        .with(eq("@42".to_string()), eq("git status".to_string()))
        .times(1)
        .return_once(|_, _| Box::pin(async { Ok(failed_tmux_output()) }));
    command_runner.expect_send_enter_key().times(0);

    // Act
    RealTmuxClient::run_command_in_window_with_runner(
        &command_runner,
        "@42".to_string(),
        "git status".to_string(),
    )
    .await;

    // Assert
    // `mockall` verifies that Enter is not sent after a failed exit status.
}

#[test]
fn parse_tmux_window_id_returns_none_for_invalid_utf8() {
    // Arrange
    let stdout = [0x80];

    // Act
    let window_id = RealTmuxClient::parse_tmux_window_id(&stdout);

    // Assert
    assert_eq!(window_id, None);
}

#[test]
fn parse_tmux_window_id_trims_newline_and_returns_window_id() {
    // Arrange
    let stdout = b"@42\n";

    // Act
    let window_id = RealTmuxClient::parse_tmux_window_id(stdout);

    // Assert
    assert_eq!(window_id, Some("@42".to_string()));
}

#[test]
fn open_window_command_builds_expected_tmux_invocation() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/agentty-session");

    // Act
    let command = ProcessTmuxCommandRunner::open_window_command(session_folder.clone());
    let (program, arguments) = command_parts(&command);

    // Assert
    assert_eq!(program, "tmux");
    assert_eq!(
        arguments,
        vec![
            "new-window".to_string(),
            "-P".to_string(),
            "-F".to_string(),
            "#{window_id}".to_string(),
            "-c".to_string(),
            session_folder.to_string_lossy().into_owned(),
        ]
    );
}

#[test]
fn send_literal_keys_command_builds_expected_tmux_invocation() {
    // Arrange
    let window_id = "@42".to_string();
    let command_text = "cargo test".to_string();

    // Act
    let command = ProcessTmuxCommandRunner::send_literal_keys_command(
        window_id.clone(),
        command_text.clone(),
    );
    let (program, arguments) = command_parts(&command);

    // Assert
    assert_eq!(program, "tmux");
    assert_eq!(
        arguments,
        vec![
            "send-keys".to_string(),
            "-t".to_string(),
            window_id,
            "-l".to_string(),
            command_text,
        ]
    );
}

#[test]
fn send_enter_key_command_builds_expected_tmux_invocation() {
    // Arrange
    let window_id = "@42".to_string();

    // Act
    let command = ProcessTmuxCommandRunner::send_enter_key_command(window_id.clone());
    let (program, arguments) = command_parts(&command);

    // Assert
    assert_eq!(program, "tmux");
    assert_eq!(
        arguments,
        vec![
            "send-keys".to_string(),
            "-t".to_string(),
            window_id,
            "C-m".to_string(),
        ]
    );
}

/// Extracts one command executable and arguments for exact CLI assertions.
fn command_parts(command: &tokio::process::Command) -> (String, Vec<String>) {
    let std_command = command.as_std();
    let program = std_command.get_program().to_string_lossy().into_owned();
    let arguments = std_command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    (program, arguments)
}

/// Builds one successful tmux subprocess output payload for tests.
fn successful_tmux_output(stdout: &[u8]) -> TmuxCommandOutput {
    TmuxCommandOutput {
        status_success: true,
        stdout: stdout.to_vec(),
    }
}

/// Builds one failed tmux subprocess output payload for tests.
fn failed_tmux_output() -> TmuxCommandOutput {
    TmuxCommandOutput {
        status_success: false,
        stdout: vec![],
    }
}
