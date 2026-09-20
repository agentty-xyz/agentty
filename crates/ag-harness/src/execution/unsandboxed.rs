//! Explicitly selected Bash execution without OS-level isolation.

use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;

use async_trait::async_trait;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdout};

use super::contract::{
    BashExecutor, BashProcess, ExecutionCommand, ExecutionError, ExecutionPolicy, MainExit,
    OutputStream, ProcessEvent,
};
use crate::command_journal::CommandCleanupScope;

/// Bash executor that runs commands with no isolation of its own, for hosts
/// that already execute inside a container, VM, or equivalent boundary.
///
/// The executor applies only launch configuration: the policy working
/// directory and the granted environment. It enforces no filesystem, network,
/// host-information, or Git-metadata boundary, and read or write grants are
/// not checked. The harness still validates policy values, persists command
/// intent before spawning, and supervises the deadline and output budget.
/// Selecting this executor is always an explicit host decision; the harness
/// never falls back to it.
pub struct UnsandboxedExecutor(());

impl UnsandboxedExecutor {
    /// Constructs the executor. The name states the contract: commands run
    /// with the harness process's own operating-system access.
    #[must_use]
    pub fn without_isolation() -> Self {
        Self(())
    }
}

impl BashExecutor for UnsandboxedExecutor {
    fn identity(&self) -> &'static str {
        "unsandboxed"
    }

    fn cleanup_scope(&self) -> CommandCleanupScope {
        CommandCleanupScope::ProcessGroupBestEffort
    }

    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError> {
        Ok(Box::new(UnsandboxedProcess {
            child: None,
            exit_delivered: false,
            group: None,
            launch: None,
            quiescent: false,
            stderr: None,
            stdout: None,
        }))
    }
}

struct UnsandboxedProcess {
    child: Option<Child>,
    exit_delivered: bool,
    group: Option<u32>,
    launch: Option<tokio::process::Command>,
    quiescent: bool,
    stderr: Option<ChildStderr>,
    stdout: Option<ChildStdout>,
}

#[async_trait]
impl BashProcess for UnsandboxedProcess {
    async fn prepare(
        &mut self,
        command: &ExecutionCommand,
        policy: &ExecutionPolicy,
    ) -> Result<(), ExecutionError> {
        let mut launch = tokio::process::Command::new(command.executable());
        launch
            .args(command.arguments())
            .current_dir(policy.workspace().join(command.directory()))
            .env_clear()
            .envs(policy.environment())
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.launch = Some(launch);

        Ok(())
    }

    async fn start(&mut self) -> Result<(), ExecutionError> {
        let mut launch = self.launch.take().ok_or(ExecutionError::Setup)?;
        let mut child = launch.spawn().map_err(|_| ExecutionError::Setup)?;
        // The spawned leader is its own process group. Its identifier must
        // survive the reap in `next_event`, where the child stops reporting
        // one, so cleanup can still signal detached descendants.
        self.group = child.id();
        self.stdout = child.stdout.take();
        self.stderr = child.stderr.take();
        self.child = Some(child);

        Ok(())
    }

    async fn next_event(&mut self, buffer: &mut [u8]) -> Result<ProcessEvent, ExecutionError> {
        let capacity = buffer.len().min(4096);
        if capacity == 0 {
            return Err(ExecutionError::Process);
        }
        if self.exit_delivered && !self.quiescent {
            // The process-group scope is best effort: the main exit is the
            // only completion this executor can acknowledge.
            self.quiescent = true;

            return Ok(ProcessEvent::Quiescent);
        }
        let mut output = [0; 4096];
        let mut errors = [0; 4096];
        tokio::select! {
            result = async { self.stdout.as_mut().ok_or(ExecutionError::Process)?.read(&mut output[..capacity]).await.map_err(|_| ExecutionError::Process) }, if self.stdout.is_some() => {
                let length = result?;
                if length == 0 {
                    self.stdout = None;
                    return Ok(ProcessEvent::Eof(OutputStream::Stdout));
                }
                buffer[..length].copy_from_slice(&output[..length]);
                Ok(ProcessEvent::Output(OutputStream::Stdout, length))
            }
            result = async { self.stderr.as_mut().ok_or(ExecutionError::Process)?.read(&mut errors[..capacity]).await.map_err(|_| ExecutionError::Process) }, if self.stderr.is_some() => {
                let length = result?;
                if length == 0 {
                    self.stderr = None;
                    return Ok(ProcessEvent::Eof(OutputStream::Stderr));
                }
                buffer[..length].copy_from_slice(&errors[..length]);
                Ok(ProcessEvent::Output(OutputStream::Stderr, length))
            }
            status = async { self.child.as_mut().ok_or(ExecutionError::Process)?.wait().await.map_err(|_| ExecutionError::Process) }, if self.child.is_some() && !self.exit_delivered => {
                self.exit_delivered = true;
                Ok(ProcessEvent::MainExit(main_exit(status?)))
            }
            // Driving an unstarted or fully acknowledged process disables
            // every branch; report a typed error instead of a select panic.
            else => Err(ExecutionError::Process)
        }
    }

    async fn cleanup(&mut self) -> Result<(), ExecutionError> {
        self.launch = None;
        self.stdout = None;
        self.stderr = None;
        if let Some(id) = self.group {
            let pid = Pid::from_raw(i32::try_from(id).map_err(|_| ExecutionError::Cleanup)?)
                .ok_or(ExecutionError::Cleanup)?;
            match kill_process_group(pid, Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(_) => return Err(ExecutionError::Cleanup),
            }
            self.group = None;
        }
        if let Some(child) = &mut self.child {
            child.wait().await.map_err(|_| ExecutionError::Cleanup)?;
            self.child = None;
        }

        Ok(())
    }
}

fn main_exit(status: std::process::ExitStatus) -> MainExit {
    status.code().map_or_else(
        || {
            status
                .signal()
                .map_or(MainExit::Unavailable, MainExit::Signal)
        },
        MainExit::Code,
    )
}

#[cfg(test)]
#[path = "unsandboxed_test.rs"]
mod tests;
