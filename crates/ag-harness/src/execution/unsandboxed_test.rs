use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use super::{UnsandboxedExecutor, main_exit};
use crate::command_journal::CommandCleanupScope;
use crate::execution::contract::{
    BashExecutor, BashProcess, ExecutionCommand, ExecutionError, ExecutionPolicy, Grants, MainExit,
    OutputStream, ProcessEvent,
};

fn policy(workspace: PathBuf, grants: Grants) -> ExecutionPolicy {
    let metadata = workspace.join("admin-root");

    ExecutionPolicy::new(workspace, vec![metadata], grants).expect("policy")
}

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("workspace");
    let path = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");

    (directory, path)
}

fn command(source: &str) -> ExecutionCommand {
    ExecutionCommand::new(
        "/bin/bash".into(),
        vec![
            "--noprofile".into(),
            "--norc".into(),
            "-c".into(),
            source.into(),
        ],
        ".".into(),
    )
    .expect("command")
}

async fn drive(process: &mut Box<dyn BashProcess>) -> (Vec<u8>, Vec<u8>, MainExit) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit = MainExit::Unavailable;
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let mut exited = false;
    let mut quiescent = false;
    let mut buffer = [0; 4096];
    while !(stdout_eof && stderr_eof && exited && quiescent) {
        assert!(Instant::now() < deadline, "command made no progress");
        match process.next_event(&mut buffer).await.expect("event") {
            ProcessEvent::Progress => {}
            ProcessEvent::Output(OutputStream::Stdout, length) => {
                stdout.extend_from_slice(&buffer[..length]);
            }
            ProcessEvent::Output(OutputStream::Stderr, length) => {
                stderr.extend_from_slice(&buffer[..length]);
            }
            ProcessEvent::Eof(OutputStream::Stdout) => stdout_eof = true,
            ProcessEvent::Eof(OutputStream::Stderr) => stderr_eof = true,
            ProcessEvent::MainExit(main_exit) => {
                exit = main_exit;
                exited = true;
            }
            ProcessEvent::Quiescent => quiescent = true,
        }
    }

    (stdout, stderr, exit)
}

#[test]
fn executor_states_identity_scope_and_inert_binding() {
    // Arrange
    let executor = UnsandboxedExecutor::without_isolation();

    // Act / Assert
    assert_eq!(executor.identity(), "unsandboxed");
    assert_eq!(
        executor.cleanup_scope(),
        CommandCleanupScope::ProcessGroupBestEffort
    );
    assert!(executor.bind().is_ok());
}

#[tokio::test]
async fn run_captures_both_streams_exit_and_granted_environment() {
    // Arrange
    let (_directory, workspace) = workspace();
    let policy = policy(
        workspace,
        Grants {
            environment: vec![("MARKER".into(), "granted-value".into())],
            ..Grants::default()
        },
    );
    let command = command("printf \"$MARKER:$PWD\"; printf diagnostics >&2; exit 7");
    let mut process = UnsandboxedExecutor::without_isolation()
        .bind()
        .expect("binding");

    // Act
    process.prepare(&command, &policy).await.expect("prepare");
    process.start().await.expect("start");
    let (stdout, stderr, exit) = drive(&mut process).await;
    let cleanup = process.cleanup().await;

    // Assert
    assert_eq!(
        String::from_utf8_lossy(&stdout),
        format!("granted-value:{}", policy.workspace().display())
    );
    assert_eq!(stderr, b"diagnostics");
    assert_eq!(exit, MainExit::Code(7));
    assert_eq!(cleanup, Ok(()));
    assert_eq!(process.cleanup().await, Ok(()), "cleanup is idempotent");
}

#[tokio::test]
async fn signal_termination_is_reported_separately_from_codes() {
    // Arrange
    let (_directory, workspace) = workspace();
    let policy = policy(workspace, Grants::default());
    let command = command("kill -TERM $$");
    let mut process = UnsandboxedExecutor::without_isolation()
        .bind()
        .expect("binding");

    // Act
    process.prepare(&command, &policy).await.expect("prepare");
    process.start().await.expect("start");
    let (_, _, exit) = drive(&mut process).await;

    // Assert
    assert_eq!(exit, MainExit::Signal(15));
    assert_eq!(process.cleanup().await, Ok(()));
}

#[tokio::test]
async fn cleanup_terminates_a_running_process_group_and_start_failures_stay_typed() {
    // Arrange
    let (_directory, workspace) = workspace();
    let running = policy(workspace.clone(), Grants::default());
    let mut process = UnsandboxedExecutor::without_isolation()
        .bind()
        .expect("binding");
    process
        .prepare(&command("/bin/sleep 30"), &running)
        .await
        .expect("prepare");
    process.start().await.expect("start");
    let mut unstarted = UnsandboxedExecutor::without_isolation()
        .bind()
        .expect("binding");
    let mut missing = UnsandboxedExecutor::without_isolation()
        .bind()
        .expect("binding");
    let absent = workspace.join("missing-executable");
    missing
        .prepare(
            &ExecutionCommand::new(absent, vec![], ".".into()).expect("command"),
            &policy(workspace, Grants::default()),
        )
        .await
        .expect("prepare");

    // Act
    let started_at = Instant::now();
    let cleanup = process.cleanup().await;

    // Assert
    assert_eq!(cleanup, Ok(()));
    assert!(
        started_at.elapsed() < Duration::from_secs(5),
        "cleanup must not wait for the command deadline"
    );
    assert_eq!(unstarted.cleanup().await, Ok(()), "cleanup before start");
    assert_eq!(unstarted.start().await, Err(ExecutionError::Setup));
    assert_eq!(missing.start().await, Err(ExecutionError::Setup));
    assert_eq!(missing.cleanup().await, Ok(()));
}

#[tokio::test]
async fn zero_capacity_buffers_are_rejected() {
    // Arrange
    let mut empty: [u8; 0] = [];
    let mut process = UnsandboxedExecutor::without_isolation()
        .bind()
        .expect("binding");

    // Act / Assert
    assert_eq!(
        process.next_event(&mut empty).await.err(),
        Some(ExecutionError::Process)
    );
}

#[test]
fn main_exit_maps_codes_signals_and_unavailable_statuses() {
    // Act / Assert
    assert_eq!(main_exit(ExitStatus::from_raw(0x0700)), MainExit::Code(7));
    assert_eq!(main_exit(ExitStatus::from_raw(9)), MainExit::Signal(9));
    assert_eq!(
        main_exit(ExitStatus::from_raw(0x137f)),
        MainExit::Unavailable
    );
}
