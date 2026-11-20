//! Bash tool unit tests for construction, executor failures and panics, and
//! command outcome reporting.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::{BashTool, command_outcome};
use crate::bash::{BashArguments, BashConfig, BashError, CommandCleanupScope, CommandTermination};
use crate::command_settlement::Commands;
use crate::effect::Effects;
use crate::execution::contract::{
    Execution, ExecutionCommand, ExecutionControl, ExecutionError, ExecutionPolicy,
    ExecutionResult, Executor, Limits, MainExit, Output, OutputStream, PreparedExecution,
    Termination,
};
use crate::fault_store_test::{Fault, FaultStore};
use crate::store::{AcquiredTurn, NewSession, SessionStore};
use crate::store_fixture::{options, schema};
use crate::{TurnError, reservation};

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

/// Executor whose commands finish at once with exit status zero and the
/// given cleanup result.
struct Finishing {
    cleanup_failure: Option<ExecutionError>,
}

impl Executor for Finishing {
    fn prepare(
        &self,
        _: ExecutionCommand,
        _: ExecutionPolicy,
        limits: Limits,
    ) -> Result<PreparedExecution, ExecutionError> {
        Ok(PreparedExecution {
            control: Box::new(Control),
            execution: Box::new(Finished {
                cleanup_failure: self.cleanup_failure,
                limits,
            }),
        })
    }
}

struct Finished {
    cleanup_failure: Option<ExecutionError>,
    limits: Limits,
}

#[async_trait]
impl Execution for Finished {
    async fn run(self: Box<Self>) -> ExecutionResult {
        ExecutionResult {
            cleanup_failure: self.cleanup_failure,
            execution_failure: None,
            main_exit: MainExit::Code(0),
            output: Output::new(self.limits),
            termination: Termination::Completed,
        }
    }
}

/// Returns a Bash tool journaling into a fresh session of `store`, with the
/// turn that owns the journal.
async fn journaled_tool(
    store: &Arc<FaultStore>,
    commands: Commands,
    effects: Effects,
    cleanup_failure: Option<ExecutionError>,
) -> (BashTool, AcquiredTurn) {
    let backend: Arc<dyn SessionStore> = store.clone();
    backend
        .create_session(&NewSession::new("bash", schema()), None, 1024)
        .await
        .expect("session");
    let acquired = reservation::acquire(
        backend,
        ("bash".into(), 0),
        "run".into(),
        options(),
        None,
        effects,
    )
    .await
    .expect("acquire");
    let tool = BashTool {
        cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
        commands,
        configuration: configuration()
            .with_environment("GREETING".into(), "hello".into())
            .expect("environment grant"),
        executor: Arc::new(Finishing { cleanup_failure }),
        external_reads: Vec::new(),
        journal: Some(acquired.guard.write_journal()),
        root: "/workspace".into(),
    };

    (tool, acquired)
}

/// Returns the native sandbox configuration, or an unsandboxed one without
/// the `native-sandbox` feature.
fn configuration() -> BashConfig {
    #[cfg(feature = "native-sandbox")]
    let configuration = BashConfig::new(
        "/trusted/launcher".into(),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(5),
        1024,
    );
    #[cfg(not(feature = "native-sandbox"))]
    let configuration = BashConfig::for_executor(
        Arc::new(crate::bash::UnsandboxedExecutor::without_isolation()),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(5),
        1024,
    );

    configuration.expect("config").with_host_information()
}

#[test]
fn construction_rejects_missing_grant_and_missing_runtime() {
    // Arrange
    let mut denied = configuration();
    denied.snapshot.host_information = false;
    // Only the native sandbox can lack its runtime launcher.
    #[cfg(feature = "native-sandbox")]
    let rejected = vec![denied, configuration()];
    #[cfg(not(feature = "native-sandbox"))]
    let rejected = vec![denied];

    // Act / Assert
    for configuration in rejected {
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
            external_reads: Vec::new(),
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
        external_reads: Vec::new(),
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

#[tokio::test]
async fn journal_failures_report_the_command_with_any_observed_outcome() {
    // Arrange
    let arguments = BashArguments::new("true".into()).expect("arguments");
    for (fault, outcome_observed) in [(Fault::CommandIntent, false), (Fault::FinishCommand, true)] {
        let store = Arc::new(FaultStore::new());
        let effects = Effects::default();
        let (tool, mut acquired) =
            journaled_tool(&store, effects.commands().clone(), effects, None).await;
        store.inject(fault);

        // Act
        let result = tool.execute(&arguments, "call").await;

        // Assert
        assert!(
            matches!(
                &result,
                Err(TurnError::CommandJournal { outcome, .. })
                    if outcome.is_some() == outcome_observed
            ),
            "{fault:?}: {result:?}"
        );
        acquired.guard.disarm();
    }
}

#[tokio::test]
async fn failed_cleanup_fails_the_journaled_command() {
    // Arrange
    let store = Arc::new(FaultStore::new());
    let effects = Effects::default();
    let (tool, mut acquired) = journaled_tool(
        &store,
        effects.commands().clone(),
        effects,
        Some(ExecutionError::Cleanup),
    )
    .await;

    // Act
    let result = tool
        .execute(
            &BashArguments::new("true".into()).expect("arguments"),
            "call",
        )
        .await;

    // Assert
    assert!(matches!(
        result,
        Err(TurnError::CommandFailed { outcome }) if outcome.cleanup_failed
    ));
    acquired.guard.disarm();
}

#[test]
fn outcome_reports_each_termination() {
    for (termination, expected) in [
        (Termination::Completed, CommandTermination::Completed),
        (Termination::Deadline, CommandTermination::Deadline),
        (Termination::Cancelled, CommandTermination::Cancelled),
        (Termination::Failed, CommandTermination::Failed),
    ] {
        // Arrange
        let limits = Limits::new(Instant::now(), Duration::from_secs(1), 4).expect("limits");

        // Act
        let outcome = command_outcome(
            &ExecutionResult {
                cleanup_failure: None,
                execution_failure: None,
                main_exit: MainExit::Code(0),
                output: Output::new(limits),
                termination,
            },
            CommandCleanupScope::ProcessGroupBestEffort,
        );

        // Assert
        assert_eq!(outcome.termination, expected);
        assert_eq!(outcome.exit_code, Some(0));
    }
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
        assert_eq!(
            (
                outcome.stdout.as_str(),
                outcome.stdout_omitted_bytes,
                outcome.stdout_tail.as_str(),
            ),
            ("ok", 0, "")
        );
        assert_eq!(
            (
                outcome.stderr.as_str(),
                outcome.stderr_omitted_bytes,
                outcome.stderr_tail.as_str(),
            ),
            ("b", 1, "d")
        );
        assert!(outcome.truncated);
    }
}
