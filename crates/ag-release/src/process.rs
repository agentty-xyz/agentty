//! Subprocess boundary for the external tools a release runs, such as `cargo`.

use std::ffi::OsString;
use std::process::Command;

/// One subprocess invocation: a program and its arguments, run in the current
/// directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessCommand {
    pub(crate) args: Vec<OsString>,
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
}

/// Host adapter that spawns real processes.
pub(crate) struct HostProcessRunner;

impl ProcessRunner for HostProcessRunner {
    fn output(&self, command: &ProcessCommand) -> Result<Vec<u8>, String> {
        let output = Command::new(&command.program)
            .args(&command.args)
            .output()
            .map_err(|error| format!("failed to run `{}`: {error}", command.display()))?;
        if !output.status.success() {
            return Err(format!(
                "`{}` failed ({}): {}",
                command.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        Ok(output.stdout)
    }
}

#[cfg(test)]
#[path = "process_test.rs"]
mod tests;
