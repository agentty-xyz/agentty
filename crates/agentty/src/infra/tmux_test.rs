use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::{env, fs, io};

use mockall::Sequence;
use mockall::predicate::eq;
use tempfile::tempdir;

use super::{
    MockTmuxCommandRunner, ProcessTmuxCommandRunner, RealTmuxClient, TmuxClient, TmuxCommandOutput,
    has_tmux_environment,
};

/// Marks the child test process that runs [`RealTmuxClient`] against a fake
/// `tmux` on an isolated `PATH`; its value is the session folder to open.
const REAL_TMUX_CLIENT_CHILD_ENV: &str = "AGENTTY_TEST_REAL_TMUX_CLIENT_CHILD";

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
fn parse_tmux_window_id_returns_none_for_invalid_utf8_or_blank_output() {
    // Arrange
    let invalid_utf8_stdout = [0x80];
    let blank_stdout = b" \n";

    // Act
    let invalid_utf8_window_id = RealTmuxClient::parse_tmux_window_id(&invalid_utf8_stdout);
    let blank_window_id = RealTmuxClient::parse_tmux_window_id(blank_stdout);

    // Assert
    assert_eq!(invalid_utf8_window_id, None);
    assert_eq!(blank_window_id, None);
}

#[tokio::test]
async fn real_tmux_client_runs_tmux_from_path_for_window_and_command() {
    if let Some(session_folder) = env::var_os(REAL_TMUX_CLIENT_CHILD_ENV) {
        // Arrange
        let tmux_client = RealTmuxClient;

        // Act
        let window_id = tmux_client
            .open_window_for_folder(PathBuf::from(session_folder))
            .await;
        tmux_client
            .run_command_in_window("@7".to_string(), "git status".to_string())
            .await;

        // Assert
        assert_eq!(window_id, Some("@7".to_string()));

        return;
    }

    // Arrange
    let command_dir = tempdir().expect("failed to create fake tmux directory");
    let invocation_log = command_dir.path().join("tmux.log");
    let session_folder = command_dir.path().join("session");
    let tmux_path = command_dir.path().join("tmux");
    fs::write(
        &tmux_path,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$AGENTTY_TEST_TMUX_LOG\"\nif [ \"$1\" = new-window \
         ]; then printf '@7\\n'; fi\n",
    )
    .expect("failed to write fake tmux command");
    let mut permissions = fs::metadata(&tmux_path)
        .expect("failed to load fake tmux metadata")
        .permissions();
    permissions.set_mode(0o750);
    fs::set_permissions(&tmux_path, permissions).expect("failed to make fake tmux executable");
    let current_test_binary = env::current_exe().expect("failed to resolve current test binary");

    // Act
    let output = tokio::process::Command::new(current_test_binary)
        .arg("--exact")
        .arg("infra::tmux::tests::real_tmux_client_runs_tmux_from_path_for_window_and_command")
        .arg("--nocapture")
        .env("PATH", command_dir.path())
        .env("AGENTTY_TEST_TMUX_LOG", &invocation_log)
        .env(REAL_TMUX_CLIENT_CHILD_ENV, &session_folder)
        .output()
        .await
        .expect("failed to run isolated tmux client test");

    // Assert
    assert!(
        output.status.success(),
        "isolated tmux client test failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&invocation_log).expect("fake tmux should record invocations"),
        format!(
            "new-window -P -F #{{window_id}} -c {}\nsend-keys -t @7 -l git status\nsend-keys -t \
             @7 C-m\n",
            session_folder.display()
        )
    );
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
