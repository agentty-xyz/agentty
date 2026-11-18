//! Host subprocess adapter unit tests.

use std::ffi::OsString;

use crate::process::{HostProcessRunner, ProcessCommand, ProcessRunner};

fn command(program: &str, args: &[&str]) -> ProcessCommand {
    ProcessCommand {
        args: args.iter().map(OsString::from).collect(),
        program: OsString::from(program),
    }
}

#[test]
fn host_runner_returns_stdout_and_reports_failures() {
    // Arrange
    let runner = HostProcessRunner;

    // Act
    let success = runner.output(&command("sh", &["-c", "printf out"]));
    let failure = runner.output(&command("sh", &["-c", "echo bad >&2; exit 3"]));
    let missing = runner.output(&command("ag-release-missing-program", &["--flag"]));

    // Assert
    assert_eq!(success, Ok(b"out".to_vec()));
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
