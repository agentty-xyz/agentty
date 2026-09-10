use std::path::PathBuf;
use std::time::Duration;

use crate::command::{
    ForgeCommand, ForgeCommandError, ForgeCommandOutput, ForgeCommandRunner,
    RealForgeCommandRunner, command_output_detail,
};

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

#[tokio::test]
async fn command_timeout_cancels_long_running_process() {
    // Arrange
    let command = ForgeCommand::new("sh", vec!["-c".to_string(), "exec sleep 5".to_string()]);
    let timeout = Duration::from_millis(25);

    // Act
    let error = RealForgeCommandRunner::run_command_with_timeout(command, timeout)
        .await
        .expect_err("long-running command should time out");

    // Assert
    assert_eq!(
        error,
        ForgeCommandError::TimedOut {
            executable: "sh".to_string(),
            timeout,
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

#[tokio::test]
async fn real_runner_applies_working_directory() {
    // Arrange
    let working_directory = std::env::current_dir().expect("test directory should exist");
    let command = ForgeCommand::new("sh", vec!["-c".to_string(), "pwd -P".to_string()])
        .with_working_directory(working_directory.clone());
    let runner = RealForgeCommandRunner;

    // Act
    let output = runner.run(command).await.expect("shell should run");

    // Assert
    assert!(output.success());
    assert_eq!(PathBuf::from(output.stdout.trim()), working_directory);
}

#[tokio::test]
async fn real_runner_reports_non_executable_path() {
    // Arrange
    let command = ForgeCommand::new(".", Vec::new());
    let runner = RealForgeCommandRunner;

    // Act
    let error = runner
        .run(command)
        .await
        .expect_err("directory cannot execute");

    // Assert
    assert!(
        matches!(error, ForgeCommandError::SpawnFailed { executable, message }
        if executable == "." && !message.is_empty())
    );
}
