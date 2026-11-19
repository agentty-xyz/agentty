//! Host subprocess adapter unit tests.

use std::ffi::OsString;

use tempfile::tempdir;

use crate::process::{HostProcessRunner, ProcessCommand, ProcessRunner};

fn command(program: &str, args: &[&str]) -> ProcessCommand {
    ProcessCommand {
        args: args.iter().map(OsString::from).collect(),
        current_dir: None,
        program: OsString::from(program),
    }
}

#[test]
fn host_runner_returns_stdout_and_reports_failures() {
    // Arrange
    let runner = HostProcessRunner;
    let directory = tempdir().expect("temporary directory");
    let in_directory = ProcessCommand {
        current_dir: Some(directory.path().to_path_buf()),
        ..command("sh", &["-c", "pwd -P"])
    };

    // Act
    let success = runner.output(&command("sh", &["-c", "printf out"]));
    let current_dir = runner.output(&in_directory);
    let failure = runner.output(&command("sh", &["-c", "echo bad >&2; exit 3"]));
    let missing = runner.output(&command("ag-release-missing-program", &["--flag"]));

    // Assert
    assert_eq!(success, Ok(b"out".to_vec()));
    let expected_dir = directory
        .path()
        .canonicalize()
        .expect("canonical directory");
    assert_eq!(
        current_dir.map(|stdout| String::from_utf8_lossy(&stdout).trim().to_string()),
        Ok(expected_dir.display().to_string())
    );
    let failure = failure.expect_err("failing command");
    assert!(
        failure.starts_with("`sh -c echo bad >&2; exit 3` failed (exit status: 3): bad"),
        "{failure}"
    );
    assert!(
        missing
            .expect_err("missing program")
            .starts_with("failed to run `ag-release-missing-program --flag`:")
    );
}

#[test]
fn host_runner_streams_output_and_reports_failures() {
    // Arrange
    let runner = HostProcessRunner;

    // Act
    let success = runner.stream(&command("sh", &["-c", "echo streamed; echo streamed >&2"]));
    let failure = runner.stream(&command("sh", &["-c", "exit 4"]));
    let missing = runner.stream(&command("ag-release-missing-program", &[]));

    // Assert
    assert_eq!(success, Ok(()));
    assert_eq!(
        failure,
        Err("`sh -c exit 4` failed (exit status: 4)".to_string())
    );
    assert!(
        missing
            .expect_err("missing program")
            .starts_with("failed to run `ag-release-missing-program`:")
    );
}
