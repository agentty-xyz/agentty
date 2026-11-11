//! Process-level contract tests for the `testty` command-line binary.

use std::process::Command;

/// Verifies the installed binary parses its arguments and dispatches the
/// selected verb, reporting a stubbed verb on stderr with a failing exit code.
#[test]
fn binary_dispatches_parsed_verb_and_reports_its_exit_code() {
    // Arrange
    let mut command = Command::new(env!("CARGO_BIN_EXE_testty"));
    command.arg("update");

    // Act
    let output = command.output().expect("run testty binary");

    // Assert
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "testty: `update` is not yet implemented\n"
    );
}
