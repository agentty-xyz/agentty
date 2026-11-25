//! Tests for session command outputs.

use crate::{SessionCommandOutput, TurnNotice};

#[test]
fn session_command_output_succeeds_only_on_exit_status_zero() {
    // Arrange
    let output = |exit_code| SessionCommandOutput {
        exit_code,
        output: String::new(),
        sandbox: TurnNotice::SandboxAvailable,
    };

    // Act / Assert
    assert!(output(Some(0)).succeeded());
    assert!(!output(Some(1)).succeeded());
    assert!(!output(None).succeeded());
}
