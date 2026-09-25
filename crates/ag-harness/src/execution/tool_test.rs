use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::{BashTool, command_outcome};
use crate::TurnError;
use crate::bash::{BashArguments, BashConfig, BashError, CommandCleanupScope, CommandTermination};
use crate::command_settlement::Commands;
use crate::execution::contract::{
    Execution, ExecutionCommand, ExecutionControl, ExecutionError, ExecutionPolicy,
    ExecutionResult, Executor, Limits, MainExit, Output, OutputStream, PreparedExecution,
    Termination,
};

struct FailingExecutor {
    panic: bool,
}

impl Executor for FailingExecutor {
    fn prepare(
        &self,
        _: ExecutionCommand,
        _: ExecutionPolicy,
        _: Limits,
    ) -> Result<PreparedExecution, ExecutionError> {
        if !self.panic {
            return Err(ExecutionError::Setup);
        }

        Ok(PreparedExecution {
            control: Box::new(Control),
            execution: Box::new(Panicking),
        })
    }
}

struct Control;

#[async_trait]
impl ExecutionControl for Control {
    fn cancel(&self) {}

    async fn cleanup(&self) -> Result<(), ExecutionError> {
        Ok(())
    }
}

struct Panicking;

#[async_trait]
impl Execution for Panicking {
    async fn run(self: Box<Self>) -> ExecutionResult {
        std::panic::resume_unwind(Box::new("executor lost its task"))
    }
}

fn configuration() -> BashConfig {
    BashConfig::new(
        "/trusted/launcher".into(),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(5),
        1024,
    )
    .expect("config")
    .with_host_information()
}

#[test]
fn construction_rejects_missing_grant_and_missing_runtime() {
    // Arrange
    let mut denied = configuration();
    denied.snapshot.host_information = false;

    // Act / Assert
    for configuration in [denied, configuration()] {
        assert!(matches!(
            BashTool::new(
                configuration,
                "/workspace".into(),
                Commands::default(),
                None
            ),
            Err(BashError::Unavailable)
        ));
    }
}

#[tokio::test]
async fn invalid_policy_and_executor_failure_never_register_effects() {
    // Arrange
    let arguments = BashArguments::new("true".into()).expect("arguments");
    for (root, executable, timeout, expected) in [
        (
            "relative",
            "/bin/bash",
            Duration::from_secs(5),
            BashError::InvalidPolicy,
        ),
        (
            "/workspace",
            "relative",
            Duration::from_secs(5),
            BashError::InvalidArguments,
        ),
        (
            "/workspace",
            "/bin/bash",
            Duration::ZERO,
            BashError::InvalidPolicy,
        ),
        (
            "/workspace",
            "/bin/bash",
            Duration::from_secs(5),
            BashError::Unavailable,
        ),
    ] {
        let mut configuration = configuration();
        configuration.snapshot.bash = executable.into();
        configuration.snapshot.timeout = timeout;
        let commands = Commands::default();
        let tool = BashTool {
            cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
            commands: commands.clone(),
            configuration,
            executor: Arc::new(FailingExecutor { panic: false }),
            journal: None,
            root: root.into(),
        };

        // Act
        let result = tool.execute(&arguments, "call").await;

        // Assert
        assert!(matches!(result, Err(TurnError::Bash(error)) if error == expected));
        assert_eq!(commands.outcomes(), Vec::new());
        commands.settled().await.expect("no effect admitted");
    }
}

#[tokio::test]
async fn executor_panic_retains_cleanup_authority_and_unknown_outcome() {
    // Arrange
    let commands = Commands::default();
    let tool = BashTool {
        cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
        commands: commands.clone(),
        configuration: configuration(),
        executor: Arc::new(FailingExecutor { panic: true }),
        journal: None,
        root: "/workspace".into(),
    };

    // Act
    let result = tool
        .execute(
            &BashArguments::new("true".into()).expect("arguments"),
            "call",
        )
        .await;

    // Assert
    assert!(matches!(result, Err(TurnError::Bash(BashError::Execution))));
    assert_eq!(commands.outcomes(), vec![None]);
    assert!(commands.settled().await.is_err());
    commands
        .retry()
        .await
        .expect("retained control recovers cleanup");
    commands.settled().await.expect("reconciled");
    assert_eq!(commands.outcomes(), vec![None]);
}

#[test]
fn outcome_keeps_exit_output_and_each_failure_dimension() {
    // Arrange
    for (failure, expected) in [
        (ExecutionError::Unsupported, BashError::Unavailable),
        (ExecutionError::Setup, BashError::Execution),
        (ExecutionError::Process, BashError::Execution),
        (ExecutionError::Supervision, BashError::Execution),
        (ExecutionError::Cleanup, BashError::Cleanup),
        (ExecutionError::CleanupUnconfirmed, BashError::Cleanup),
    ] {
        let mut output =
            Output::new(Limits::new(Instant::now(), Duration::from_secs(1), 4).expect("limits"));
        output.capture(OutputStream::Stdout, b"ok");
        output.capture(OutputStream::Stderr, b"bad");

        // Act
        let outcome = command_outcome(
            &ExecutionResult {
                cleanup_failure: Some(ExecutionError::Cleanup),
                execution_failure: Some(failure),
                main_exit: MainExit::Signal(15),
                output,
                termination: Termination::Failed,
            },
            CommandCleanupScope::ProcessGroupBestEffort,
        );

        // Assert
        assert_eq!(outcome.execution_failure, Some(expected));
        assert!(outcome.cleanup_failed);
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.signal, Some(15));
        assert_eq!(outcome.termination, CommandTermination::Failed);
        assert_eq!(outcome.stdout, "ok");
        assert_eq!(outcome.stderr, "ba");
        assert!(outcome.truncated);
    }
}
