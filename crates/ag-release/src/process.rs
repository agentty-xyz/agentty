//! Subprocess boundary for the external tools a release runs, such as `cargo`
//! and `git`.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

/// One subprocess invocation: a program, its arguments, and the directory it
/// runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessCommand {
    pub(crate) args: Vec<OsString>,
    /// Working directory, or `None` for the current directory.
    pub(crate) current_dir: Option<PathBuf>,
    pub(crate) program: OsString,
}

impl ProcessCommand {
    /// Returns the command line for error messages.
    pub(crate) fn display(&self) -> String {
        std::iter::once(&self.program)
            .chain(&self.args)
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Returns a `std` command for this invocation.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        if let Some(current_dir) = &self.current_dir {
            command.current_dir(current_dir);
        }

        command
    }

    /// Returns the error for a command that could not start.
    fn start_error(&self, error: &io::Error) -> String {
        format!("failed to run `{}`: {error}", self.display())
    }

    /// Returns the error for a command that exited with `status`, followed by
    /// `detail` when it is nonempty.
    fn status_error(&self, status: ExitStatus, detail: &str) -> String {
        let summary = format!("`{}` failed ({status})", self.display());
        if detail.is_empty() {
            return summary;
        }

        format!("{summary}: {detail}")
    }
}

/// Runs subprocesses so orchestration code can be tested with deterministic
/// mocks.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait ProcessRunner {
    /// Runs `command` to completion and returns its standard output.
    ///
    /// # Errors
    /// Returns an error naming the command when it cannot start or exits
    /// unsuccessfully, including its standard error.
    fn output(&self, command: &ProcessCommand) -> Result<Vec<u8>, String>;

    /// Runs `command` to completion with its standard output and standard
    /// error streamed to this process's standard error, so long-running tools
    /// such as `cargo build` stay visible without polluting standard output.
    ///
    /// # Errors
    /// Returns an error naming the command when it cannot start or exits
    /// unsuccessfully.
    fn stream(&self, command: &ProcessCommand) -> Result<(), String>;
}

/// Host adapter that spawns real processes.
pub(crate) struct HostProcessRunner;

impl ProcessRunner for HostProcessRunner {
    fn output(&self, command: &ProcessCommand) -> Result<Vec<u8>, String> {
        let output = command
            .command()
            .output()
            .map_err(|error| command.start_error(&error))?;
        if !output.status.success() {
            return Err(command.status_error(
                output.status,
                String::from_utf8_lossy(&output.stderr).trim(),
            ));
        }

        Ok(output.stdout)
    }

    fn stream(&self, command: &ProcessCommand) -> Result<(), String> {
        let status = command
            .command()
            .stdin(Stdio::null())
            .stdout(io::stderr())
            .status()
            .map_err(|error| command.start_error(&error))?;
        if !status.success() {
            return Err(command.status_error(status, ""));
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "process_test.rs"]
mod tests;
