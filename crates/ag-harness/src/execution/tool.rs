//! Bash consumes the shared supervisor over the selected executor and
//! retains its journal and control.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[cfg(feature = "native-sandbox")]
use super::contract::BashExecutor;
use super::contract::{
    ExecutionCommand, ExecutionControl, ExecutionError, ExecutionPolicy, ExecutionResult, Executor,
    Grants, Limits, MainExit, Termination,
};
#[cfg(feature = "native-sandbox")]
use super::native::Native;
use super::supervisor::{Monotonic, Supervisor};
use crate::TurnError;
use crate::bash::{BashArguments, BashConfig, BashError, Capability};
use crate::command_journal::{
    CommandCleanupScope, CommandIntent, CommandOutcome, CommandTermination,
};
use crate::command_settlement::Commands;
use crate::reservation::WriteJournal;

pub(crate) struct BashTool {
    cleanup_scope: CommandCleanupScope,
    commands: Commands,
    configuration: BashConfig,
    executor: Arc<dyn Executor>,
    /// Host read grants followed by the system directories present on this
    /// host when the configuration grants them, resolved once per turn.
    external_reads: Vec<PathBuf>,
    journal: Option<WriteJournal>,
    root: PathBuf,
}

impl BashTool {
    pub(crate) fn new(
        configuration: BashConfig,
        root: PathBuf,
        commands: Commands,
        journal: Option<WriteJournal>,
    ) -> Result<Self, BashError> {
        if !configuration.snapshot.host_information {
            return Err(BashError::Unavailable);
        }
        #[cfg(feature = "native-sandbox")]
        let selected: Arc<dyn BashExecutor> = match &configuration.executor {
            Some(executor) => Arc::clone(executor),
            None => Arc::new(Native {
                configuration: configuration.clone(),
            }),
        };
        // Without the native sandbox, `BashConfig::for_executor` is the only
        // constructor, so the executor is always host-selected.
        #[cfg(not(feature = "native-sandbox"))]
        let selected = configuration
            .executor
            .clone()
            .ok_or(BashError::Unavailable)?;
        let cleanup_scope = selected.cleanup_scope();
        let supervisor =
            Supervisor::new(selected, Arc::new(Monotonic)).map_err(|_| BashError::Unavailable)?;
        let mut external_reads = configuration.snapshot.external_reads.clone();
        if configuration.snapshot.grants(Capability::SystemReads) {
            external_reads.extend(super::system::reads());
        }

        Ok(Self {
            cleanup_scope,
            commands,
            configuration,
            executor: Arc::new(supervisor),
            external_reads,
            journal,
            root,
        })
    }

    pub(crate) async fn execute(
        &self,
        arguments: &BashArguments,
        call_id: &str,
    ) -> Result<CommandOutcome, TurnError> {
        let configuration = &self.configuration.snapshot;
        let grants = Grants {
            environment: self
                .configuration
                .environment
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value)))
                .chain(
                    configuration
                        .temporary_directory
                        .iter()
                        .map(|path| (OsString::from("TMPDIR"), path.clone().into_os_string())),
                )
                .collect(),
            external_reads: self.external_reads.clone(),
            git_writes: configuration.grants(Capability::GitWrites),
            host_information: configuration.host_information,
            network: configuration.grants(Capability::Network),
            temporary_directory: configuration.temporary_directory.clone(),
            workspace_writes: configuration.workspace_writes.clone(),
        };
        let policy = ExecutionPolicy::new(self.root.clone(), vec![self.root.join(".git")], grants)
            .map_err(|_| BashError::InvalidPolicy)?;
        let command = ExecutionCommand::new(
            configuration.bash.clone(),
            vec![
                "--noprofile".into(),
                "--norc".into(),
                "-c".into(),
                arguments.command().into(),
            ],
            ".".into(),
        )
        .map_err(|_| BashError::InvalidArguments)?;
        let limits = Limits::new(
            Instant::now(),
            configuration.timeout,
            configuration.capture_bytes,
        )
        .map_err(|_| BashError::InvalidPolicy)?;
        let prepared = self
            .executor
            .prepare(command, policy, limits)
            .map_err(|_| BashError::Unavailable)?;
        let control: Arc<dyn ExecutionControl> = prepared.control.into();
        let _cancellation = Cancel(Arc::clone(&control));
        let operation = self.commands.register(control, self.journal.clone());
        let lease = self.commands.retain();
        let intent = CommandIntent {
            call_id: call_id.into(),
            command: arguments.command().into(),
            policy: self.configuration.fingerprint(),
            workspace: self.root.clone(),
        };
        let cleanup_scope = self.cleanup_scope;
        let journal = self.journal.clone();
        tokio::spawn(async move {
            let _lease = lease;
            let id = match journal.as_ref() {
                Some(journal) => Some(journal.command_intent(&intent).await.map_err(|source| {
                    TurnError::CommandJournal {
                        outcome: None,
                        source: Box::new(source),
                    }
                })?),
                None => None,
            };
            operation.admitted(id);
            let result = prepared.execution.run().await;
            let outcome = command_outcome(&result, cleanup_scope);
            operation.observed(outcome.clone());
            operation
                .persist()
                .await
                .map_err(|source| TurnError::CommandJournal {
                    outcome: Some(Box::new(outcome.clone())),
                    source: Box::new(source),
                })?;
            if outcome.cleanup_failed || outcome.execution_failure.is_some() {
                return Err(TurnError::CommandFailed {
                    outcome: Box::new(outcome),
                });
            }

            Ok(outcome)
        })
        .await
        .map_err(|_| BashError::Execution)?
    }
}

struct Cancel(Arc<dyn ExecutionControl>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn command_outcome(result: &ExecutionResult, cleanup_scope: CommandCleanupScope) -> CommandOutcome {
    let stdout = result.output.stdout();
    let stderr = result.output.stderr();

    CommandOutcome {
        cleanup_failed: result.cleanup_failure.is_some(),
        cleanup_scope,
        execution_failure: result.execution_failure.map(|error| match error {
            ExecutionError::Unsupported => BashError::Unavailable,
            ExecutionError::Setup | ExecutionError::Process | ExecutionError::Supervision => {
                BashError::Execution
            }
            ExecutionError::Cleanup | ExecutionError::CleanupUnconfirmed => BashError::Cleanup,
        }),
        exit_code: if let MainExit::Code(code) = result.main_exit {
            Some(code)
        } else {
            None
        },
        signal: if let MainExit::Signal(signal) = result.main_exit {
            Some(signal)
        } else {
            None
        },
        stderr: String::from_utf8_lossy(&stderr.head).into_owned(),
        stderr_omitted_bytes: stderr.omitted_bytes,
        stderr_tail: String::from_utf8_lossy(&stderr.tail).into_owned(),
        stdout: String::from_utf8_lossy(&stdout.head).into_owned(),
        stdout_omitted_bytes: stdout.omitted_bytes,
        stdout_tail: String::from_utf8_lossy(&stdout.tail).into_owned(),
        termination: match result.termination {
            Termination::Completed => CommandTermination::Completed,
            Termination::Deadline => CommandTermination::Deadline,
            Termination::Cancelled => CommandTermination::Cancelled,
            Termination::Failed => CommandTermination::Failed,
        },
        truncated: result.output.truncated(),
    }
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
