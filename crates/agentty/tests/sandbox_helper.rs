//! The `agentty` executable doubles as the native sandbox helper of its
//! Harness sessions.

use std::process::Stdio;

/// First argument a harness starts its sandbox helper with.
const HELPER_ARGUMENT: &str = "--ag-harness-sandbox";

#[tokio::test]
async fn helper_start_runs_the_sandbox_helper_instead_of_the_application() {
    // Arrange
    let root = tempfile::tempdir().expect("root");

    // Act
    // Without the harness's private configuration channel, the helper
    // reports failure and exits.
    let output = tokio::process::Command::new(assert_cmd::cargo::cargo_bin!("agentty"))
        .arg(HELPER_ARGUMENT)
        .env("AGENTTY_ROOT", root.path())
        .stdin(Stdio::null())
        .output()
        .await
        .expect("helper output");

    // Assert
    // The application would reject the unknown argument with usage exit
    // code 2 and draw the terminal UI on stdout, and would create its
    // database and lock under the Agentty root.
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
    assert_eq!(
        std::fs::read_dir(root.path())
            .expect("root entries")
            .count(),
        0
    );
}
