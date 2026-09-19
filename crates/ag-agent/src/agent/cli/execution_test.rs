use std::ffi::OsString;
use std::io;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::Duration;

use ag_contracts::{AgentRequestKind, ReasoningLevel};
use ag_protocol::TurnPromptAttachment;
use ag_session::AgentKind;
use rustix::process::{self, Pid, WaitId, WaitIdOptions};
use tempfile::tempdir;
use tokio::io::{AsyncRead, ReadBuf};

use crate::agent::backend::{AgentBackendError, BuildCommandRequest, MockAgentBackend};
use crate::agent::cli::execution::{
    CliExecutionError, CliExecutionObserver, CliExitStatus, CollectingCliObserver,
    ProcessGroupGuard, capture_stderr, capture_stdout, execute_cli_command, finish_cli_execution,
    require_pipe,
};

/// Observer that records all streaming callbacks for assertions.
struct RecordingObserver {
    lines: Mutex<Vec<String>>,
    pid_updates: Mutex<Vec<Option<u32>>>,
}

impl RecordingObserver {
    /// Creates an empty callback recorder.
    fn new() -> Self {
        Self {
            lines: Mutex::new(Vec::new()),
            pid_updates: Mutex::new(Vec::new()),
        }
    }
}

impl CliExecutionObserver for RecordingObserver {
    fn pid_updated(&self, child_pid: Option<u32>) {
        self.pid_updates
            .lock()
            .expect("PID update lock should be available")
            .push(child_pid);
    }

    fn stdout_line(&self, line: &str) {
        self.lines
            .lock()
            .expect("line lock should be available")
            .push(line.to_string());
    }
}

/// Async reader that deterministically fails every read.
struct FailingReader;

impl AsyncRead for FailingReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("read failed")))
    }
}

/// Builds one borrowed command request for executor tests.
fn build_request<'a>(
    attachments: &'a [TurnPromptAttachment],
    folder: &'a Path,
    prompt: &'a str,
    request_kind: &'a AgentRequestKind,
) -> BuildCommandRequest<'a> {
    BuildCommandRequest {
        attachments,
        folder,
        main_checkout_root: None,
        model: "test-model",
        permission_mode: ag_contracts::PermissionMode::AutoEdit,
        personality_prompt: None,
        prompt,
        reasoning_level: ReasoningLevel::default(),
        request_kind,
        speed_mode: ag_contracts::SpeedMode::default(),
        replay_transcript: None,
    }
}

/// Builds a shell command for deterministic subprocess output.
fn shell_command(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(script);

    command
}

#[tokio::test]
async fn test_execute_cli_command_returns_command_build_failure() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let request_kind = AgentRequestKind::UtilityPrompt;
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        Err(AgentBackendError::CommandBuild(
            "command unavailable".to_string(),
        ))
    });

    // Act
    let error = execute_cli_command(
        &backend,
        AgentKind::Codex,
        build_request(&[], folder.path(), "prompt", &request_kind),
        &CollectingCliObserver,
        None,
    )
    .await
    .expect_err("command construction should fail");

    // Assert
    assert!(matches!(
        error,
        CliExecutionError::CommandBuild(AgentBackendError::CommandBuild(message))
            if message == "command unavailable"
    ));
}

#[tokio::test]
async fn test_execute_cli_command_returns_stdin_build_failure() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let request_kind = AgentRequestKind::UtilityPrompt;
    let attachments = vec![TurnPromptAttachment {
        local_image_path: PathBuf::from(OsString::from_vec(b"/tmp/image-\xff.png".to_vec())),
        placeholder: "[Image #1]".to_string(),
    }];
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .returning(|_| Ok(shell_command("exit 0")));

    // Act
    let error = execute_cli_command(
        &backend,
        AgentKind::Claude,
        build_request(
            &attachments,
            folder.path(),
            "Review [Image #1]",
            &request_kind,
        ),
        &CollectingCliObserver,
        None,
    )
    .await
    .expect_err("stdin rendering should fail");

    // Assert
    assert!(matches!(error, CliExecutionError::StdinBuild(_)));
}

#[tokio::test]
async fn test_execute_cli_command_returns_spawn_failure() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let request_kind = AgentRequestKind::UtilityPrompt;
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .returning(|_| Ok(Command::new("/no-such-binary-agentty-cli-execution-test")));

    // Act
    let error = execute_cli_command(
        &backend,
        AgentKind::Codex,
        build_request(&[], folder.path(), "prompt", &request_kind),
        &CollectingCliObserver,
        None,
    )
    .await
    .expect_err("process spawning should fail");

    // Assert
    assert!(matches!(error, CliExecutionError::Spawn(_)));
}

#[tokio::test]
async fn test_execute_cli_command_collects_output_and_streams_lines() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let request_kind = AgentRequestKind::UtilityPrompt;
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().returning(|_| {
        Ok(shell_command(
            "printf 'first\\nsecond'; printf 'warning' >&2",
        ))
    });
    let observer = RecordingObserver::new();

    // Act
    let output = execute_cli_command(
        &backend,
        AgentKind::Codex,
        build_request(&[], folder.path(), "prompt", &request_kind),
        &observer,
        None,
    )
    .await
    .expect("process should complete");

    // Assert
    assert_eq!(output.exit_status, CliExitStatus::Success);
    assert_eq!(output.stdout, "first\nsecond");
    assert_eq!(output.stderr, "warning");
    assert_eq!(
        *observer
            .lines
            .lock()
            .expect("line lock should be available"),
        vec!["first".to_string(), "second".to_string()]
    );
    let pid_updates = observer
        .pid_updates
        .lock()
        .expect("PID update lock should be available");
    assert!(pid_updates.first().is_some_and(Option::is_some));
    assert_eq!(pid_updates.last(), Some(&None));
}

#[tokio::test]
async fn test_parent_exit_terminates_descendants_before_draining_output() {
    for (status, expected_status) in [
        (0, CliExitStatus::Success),
        (7, CliExitStatus::NonZero(Some(7))),
    ] {
        // Arrange: the background child inherits both output pipes and outlives
        // the non-exec parent unless the executor terminates its process group.
        let folder = tempdir().expect("temporary folder should be created");
        let request_kind = AgentRequestKind::UtilityPrompt;
        let mut backend = MockAgentBackend::new();
        backend.expect_build_command().once().returning(move |_| {
            Ok(shell_command(&format!(
                "sleep 60 & printf 'first\\nlast'; printf 'warning' >&2; exit {status}"
            )))
        });
        let observer = RecordingObserver::new();

        // Act: no execution deadline; the outer timeout only bounds this test.
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            execute_cli_command(
                &backend,
                AgentKind::Codex,
                build_request(&[], folder.path(), "prompt", &request_kind),
                &observer,
                None,
            ),
        )
        .await
        .expect("parent exit must release descendant-held pipes promptly")
        .expect("process output should be collected");

        // Assert: preserve parent status and buffered output, including a final
        // stdout line without a newline, despite terminating its descendants.
        assert_eq!(output.exit_status, expected_status);
        assert_eq!(output.stdout, "first\nlast");
        assert_eq!(output.stderr, "warning");
        assert_eq!(
            *observer
                .lines
                .lock()
                .expect("line lock should be available"),
            ["first", "last"]
        );
        assert_eq!(
            observer
                .pid_updates
                .lock()
                .expect("PID lock should be available")
                .last(),
            Some(&None)
        );
    }
}

#[tokio::test]
async fn test_execute_cli_command_disables_optional_git_locks_for_tools() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let request_kind = AgentRequestKind::UtilityPrompt;
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().once().returning(|_| {
        let mut command = shell_command("sh -c 'printf %s \"$GIT_OPTIONAL_LOCKS\"'");
        command.env("GIT_OPTIONAL_LOCKS", "1");

        Ok(command)
    });

    // Act
    let output = execute_cli_command(
        &backend,
        AgentKind::Codex,
        build_request(&[], folder.path(), "prompt", &request_kind),
        &CollectingCliObserver,
        None,
    )
    .await
    .expect("process should complete");

    // Assert
    assert_eq!(output.exit_status, CliExitStatus::Success);
    assert_eq!(output.stdout, "0");
}

#[tokio::test]
async fn test_execute_cli_command_classifies_non_zero_exit() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let request_kind = AgentRequestKind::UtilityPrompt;
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .returning(|_| Ok(shell_command("exit 7")));

    // Act
    let output = execute_cli_command(
        &backend,
        AgentKind::Codex,
        build_request(&[], folder.path(), "prompt", &request_kind),
        &CollectingCliObserver,
        None,
    )
    .await
    .expect("non-zero termination should still return raw output");

    // Assert
    assert_eq!(output.exit_status, CliExitStatus::NonZero(Some(7)));
}

#[tokio::test]
async fn test_execute_cli_command_classifies_signal_termination() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let request_kind = AgentRequestKind::UtilityPrompt;
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .returning(|_| Ok(shell_command("kill -9 $$")));

    // Act
    let output = execute_cli_command(
        &backend,
        AgentKind::Codex,
        build_request(&[], folder.path(), "prompt", &request_kind),
        &CollectingCliObserver,
        None,
    )
    .await
    .expect("signal termination should still return raw output");

    // Assert
    assert_eq!(output.exit_status, CliExitStatus::Signaled(9));
}

#[tokio::test]
async fn test_execute_cli_command_enforces_timeout() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");
    let request_kind = AgentRequestKind::UtilityPrompt;
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .returning(|_| Ok(shell_command("while :; do :; done")));

    // Act
    let error = execute_cli_command(
        &backend,
        AgentKind::Codex,
        build_request(&[], folder.path(), "prompt", &request_kind),
        &CollectingCliObserver,
        Some(Duration::from_millis(20)),
    )
    .await
    .expect_err("process should time out");

    // Assert
    assert!(matches!(error, CliExecutionError::Timeout(_)));
}

#[tokio::test]
async fn test_finish_cli_execution_aborts_stdin_writer_after_execution_error() {
    // Arrange
    let stdin_write_task = tokio::spawn(std::future::pending());
    let execution_result = Err(CliExecutionError::StdoutRead(io::Error::other(
        "read failed",
    )));

    // Act
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        finish_cli_execution(Some(stdin_write_task), execution_result),
    )
    .await
    .expect("stdin writer cleanup should not hang")
    .expect_err("execution failure should be preserved");

    // Assert
    assert!(matches!(error, CliExecutionError::StdoutRead(_)));
}

#[test]
fn test_require_pipe_returns_unavailable_error() {
    // Arrange / Act
    let error = require_pipe::<u8>(None, "stdout").expect_err("pipe should be required");

    // Assert
    assert!(matches!(
        error,
        CliExecutionError::PipeUnavailable("stdout")
    ));
}

#[tokio::test]
async fn test_capture_stdout_returns_read_error() {
    // Arrange
    let observer = CollectingCliObserver;

    // Act
    let error = capture_stdout(tokio::io::BufReader::new(FailingReader), &observer)
        .await
        .expect_err("stdout read should fail");

    // Assert
    assert!(matches!(error, CliExecutionError::StdoutRead(_)));
}

#[tokio::test]
async fn test_capture_stderr_returns_read_error() {
    // Arrange / Act
    let error = capture_stderr(FailingReader)
        .await
        .expect_err("stderr read should fail");

    // Assert
    assert!(matches!(error, CliExecutionError::StderrRead(_)));
}

#[tokio::test]
async fn cleanup_without_process_group_ownership_does_not_signal_the_host() {
    // Arrange
    let mut guard = ProcessGroupGuard(None);
    let host = std::process::id();

    // Act
    guard.wait_for_exit().await.expect("no leader to wait for");
    drop(guard);

    // Assert: reaching this point proves no signal was sent to the host group.
    assert_eq!(std::process::id(), host);
}

#[tokio::test]
async fn group_cleanup_retains_the_exited_leader_until_reaping() {
    // Arrange
    let mut child = tokio::process::Command::from(shell_command("exit 7"))
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn leader");
    let leader = Pid::from_raw(i32::try_from(child.id().expect("child PID")).expect("valid PID"))
        .expect("nonzero PID");
    let mut guard = ProcessGroupGuard(Some(leader));

    // Act
    tokio::time::timeout(Duration::from_secs(5), guard.wait_for_exit())
        .await
        .expect("leader exits promptly")
        .expect("observe exit");
    drop(guard);
    let retained = process::waitid(
        WaitId::Pid(leader),
        WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
    )
    .expect("leader still belongs to this executor");
    let status = child.wait().await.expect("reap leader");

    // Assert
    assert!(retained.is_some(), "cleanup must leave the leader unreaped");
    assert_eq!(status.code(), Some(7));
}

#[tokio::test]
async fn canceling_exit_observation_still_terminates_the_owned_group() {
    // Arrange
    let mut child = tokio::process::Command::from(shell_command("while :; do :; done"))
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn leader");
    let leader = Pid::from_raw(i32::try_from(child.id().expect("child PID")).expect("valid PID"))
        .expect("nonzero PID");
    let mut guard = ProcessGroupGuard(Some(leader));

    // Act
    let observation = tokio::time::timeout(Duration::from_millis(20), guard.wait_for_exit()).await;
    drop(guard);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("cleanup terminates the leader")
        .expect("reap leader");

    // Assert
    assert!(observation.is_err());
    assert!(!status.success());
}

#[tokio::test]
async fn lost_leader_ownership_disarms_group_cleanup() {
    // Arrange
    let mut child = tokio::process::Command::from(shell_command("exit 0"))
        .process_group(0)
        .spawn()
        .expect("spawn leader");
    let leader = Pid::from_raw(i32::try_from(child.id().expect("child PID")).expect("valid PID"))
        .expect("nonzero PID");
    let mut guard = ProcessGroupGuard(Some(leader));
    child.wait().await.expect("simulate another waiter reaping");

    // Act
    let error = guard
        .wait_for_exit()
        .await
        .expect_err("leader was already reaped");

    // Assert
    assert_eq!(
        error.raw_os_error(),
        Some(rustix::io::Errno::CHILD.raw_os_error())
    );
    assert!(
        guard.0.is_none(),
        "never signal a reusable process group ID"
    );
}
