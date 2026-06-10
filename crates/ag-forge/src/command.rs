//! Forge CLI command boundary used by review-request adapters.

use std::io::ErrorKind;
use std::path::PathBuf;

use tokio::process::Command;

use super::ForgeFuture;

/// One forge CLI invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForgeCommand {
    /// Argument vector passed to the executable.
    pub(crate) arguments: Vec<String>,
    /// Environment variables applied to the spawned process.
    pub(crate) environment: Vec<(String, String)>,
    /// Executable name passed to the OS process launcher.
    pub(crate) executable: &'static str,
    /// Working directory used for one repository-aware forge command.
    pub(crate) working_directory: Option<PathBuf>,
}

impl ForgeCommand {
    /// Builds one forge CLI command with no extra environment.
    pub(crate) fn new(executable: &'static str, arguments: Vec<String>) -> Self {
        Self {
            arguments,
            environment: Vec::new(),
            executable,
            working_directory: None,
        }
    }

    /// Adds one environment variable to the command.
    pub(crate) fn with_environment(mut self, key: &str, value: impl Into<String>) -> Self {
        self.environment.push((key.to_string(), value.into()));

        self
    }

    /// Sets the working directory for one repository-aware forge command.
    pub(crate) fn with_working_directory(mut self, working_directory: PathBuf) -> Self {
        self.working_directory = Some(working_directory);

        self
    }

    /// Sets the working directory when one repository path is available.
    pub(crate) fn with_optional_working_directory(
        self,
        working_directory: Option<PathBuf>,
    ) -> Self {
        match working_directory {
            Some(working_directory) => self.with_working_directory(working_directory),
            None => self,
        }
    }
}

/// Raw process output captured from one forge CLI invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForgeCommandOutput {
    /// Process exit code, or `None` when the process terminated without one.
    pub(crate) exit_code: Option<i32>,
    /// Captured standard error text.
    pub(crate) stderr: String,
    /// Captured standard output text.
    pub(crate) stdout: String,
}

impl ForgeCommandOutput {
    /// Returns whether the command exited successfully.
    pub(crate) fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Spawn-time failures before a forge CLI command can complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ForgeCommandError {
    /// The requested executable was not found on the local machine.
    ExecutableNotFound { executable: String },
    /// The process could not be started for another reason.
    SpawnFailed {
        /// Executable name that failed to spawn.
        executable: String,
        /// Human-readable spawn error detail.
        message: String,
    },
}

/// Async command boundary used by forge adapters.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait ForgeCommandRunner: Send + Sync {
    /// Runs one forge CLI command and returns the captured output.
    fn run(
        &self,
        command: ForgeCommand,
    ) -> ForgeFuture<Result<ForgeCommandOutput, ForgeCommandError>>;
}

/// Production [`ForgeCommandRunner`] backed by `tokio::process::Command`.
pub(crate) struct RealForgeCommandRunner;

impl ForgeCommandRunner for RealForgeCommandRunner {
    fn run(
        &self,
        command: ForgeCommand,
    ) -> ForgeFuture<Result<ForgeCommandOutput, ForgeCommandError>> {
        Box::pin(async move { run_command(command).await })
    }
}

/// Runs one forge CLI command and captures stdout, stderr, and exit status.
async fn run_command(command: ForgeCommand) -> Result<ForgeCommandOutput, ForgeCommandError> {
    let mut process = Command::new(command.executable);
    process.args(&command.arguments);

    for (key, value) in &command.environment {
        process.env(key, value);
    }

    if let Some(working_directory) = &command.working_directory {
        process.current_dir(working_directory);
    }

    let output = process.output().await.map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            return ForgeCommandError::ExecutableNotFound {
                executable: command.executable.to_string(),
            };
        }

        ForgeCommandError::SpawnFailed {
            executable: command.executable.to_string(),
            message: error.to_string(),
        }
    })?;

    Ok(ForgeCommandOutput {
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// Extracts the best human-readable error detail from command output.
pub(crate) fn command_output_detail(output: &ForgeCommandOutput) -> String {
    let stderr_text = output.stderr.trim();
    if !stderr_text.is_empty() {
        return stderr_text.to_string();
    }

    let stdout_text = output.stdout.trim();
    if !stdout_text.is_empty() {
        return stdout_text.to_string();
    }

    "Unknown forge CLI error".to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn forge_command_builders_set_environment_and_working_directory() {
        // Arrange
        let working_directory = PathBuf::from("/tmp/repo");

        // Act
        let command = ForgeCommand::new("gh", vec!["pr".to_string(), "view".to_string()])
            .with_environment("GH_TOKEN", "secret")
            .with_working_directory(working_directory.clone());

        // Assert
        assert_eq!(command.executable, "gh");
        assert_eq!(command.arguments, vec!["pr", "view"]);
        assert_eq!(
            command.environment,
            vec![("GH_TOKEN".to_string(), "secret".to_string())]
        );
        assert_eq!(command.working_directory, Some(working_directory));
    }

    #[test]
    fn forge_command_optional_working_directory_leaves_command_unchanged_when_missing() {
        // Arrange
        let command = ForgeCommand::new("glab", vec!["mr".to_string(), "view".to_string()]);

        // Act
        let command = command.with_optional_working_directory(None);

        // Assert
        assert_eq!(command.working_directory, None);
    }

    #[test]
    fn forge_command_output_success_requires_zero_exit_code() {
        // Arrange
        let successful_output = ForgeCommandOutput {
            exit_code: Some(0),
            stderr: String::new(),
            stdout: String::new(),
        };
        let failed_output = ForgeCommandOutput {
            exit_code: Some(1),
            stderr: String::new(),
            stdout: String::new(),
        };
        let signaled_output = ForgeCommandOutput {
            exit_code: None,
            stderr: String::new(),
            stdout: String::new(),
        };

        // Act
        let successful = successful_output.success();
        let failed = failed_output.success();
        let signaled = signaled_output.success();

        // Assert
        assert!(successful);
        assert!(!failed);
        assert!(!signaled);
    }

    #[tokio::test]
    async fn real_runner_captures_stdout_stderr_and_exit_code() {
        // Arrange
        let command = ForgeCommand::new(
            "sh",
            vec![
                "-c".to_string(),
                "printf '%s' \"$FORGE_STDOUT\"; printf '%s' err >&2; exit 7".to_string(),
            ],
        )
        .with_environment("FORGE_STDOUT", "out");
        let runner = RealForgeCommandRunner;

        // Act
        let output = runner
            .run(command)
            .await
            .expect("shell command should run and return captured output");

        // Assert
        assert_eq!(
            output,
            ForgeCommandOutput {
                exit_code: Some(7),
                stderr: "err".to_string(),
                stdout: "out".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn real_runner_reports_missing_executable() {
        // Arrange
        let command = ForgeCommand::new(
            "agentty-definitely-missing-forge-command",
            vec!["version".to_string()],
        );
        let runner = RealForgeCommandRunner;

        // Act
        let error = runner
            .run(command)
            .await
            .expect_err("missing executable should be reported before output capture");

        // Assert
        assert_eq!(
            error,
            ForgeCommandError::ExecutableNotFound {
                executable: "agentty-definitely-missing-forge-command".to_string(),
            }
        );
    }

    #[test]
    fn command_output_detail_prefers_trimmed_stderr_then_stdout() {
        // Arrange
        let stderr_output = ForgeCommandOutput {
            exit_code: Some(1),
            stderr: "  stderr detail\n".to_string(),
            stdout: "stdout detail".to_string(),
        };
        let stdout_output = ForgeCommandOutput {
            exit_code: Some(1),
            stderr: "  \n".to_string(),
            stdout: "  stdout detail\n".to_string(),
        };

        // Act
        let stderr_detail = command_output_detail(&stderr_output);
        let stdout_detail = command_output_detail(&stdout_output);

        // Assert
        assert_eq!(stderr_detail, "stderr detail");
        assert_eq!(stdout_detail, "stdout detail");
    }

    #[test]
    fn command_output_detail_falls_back_when_output_is_blank() {
        // Arrange
        let output = ForgeCommandOutput {
            exit_code: Some(1),
            stderr: "  \n".to_string(),
            stdout: "\t".to_string(),
        };

        // Act
        let detail = command_output_detail(&output);

        // Assert
        assert_eq!(detail, "Unknown forge CLI error");
    }
}
