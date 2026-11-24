//! Tests for interpreting commit hook runs.

use crate::error::GitError;
use crate::hook::HookOutput;

#[test]
fn hook_output_recognizes_rewrites_and_reports_rejections() {
    // Arrange
    let rewritten = HookOutput {
        output: "Files were modified by this hook\n".to_string(),
        success: false,
    };
    let rejected = HookOutput {
        output: "lint failed\n".to_string(),
        success: false,
    };
    let accepted = HookOutput {
        output: "files were modified by this hook".to_string(),
        success: true,
    };

    // Act
    let rejection = rejected.rejection("git commit");

    // Assert
    assert!(rewritten.modified_files());
    assert!(!rejected.modified_files());
    assert!(!accepted.modified_files());
    assert!(matches!(
        rejection,
        GitError::CommandFailed { ref command, ref stderr }
            if command == "git commit" && stderr == "lint failed"
    ));
}
