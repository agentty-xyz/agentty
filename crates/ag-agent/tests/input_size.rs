//! Public contract for recognizing provider input-size diagnostics.

use ag_agent::is_input_size_error;

#[test]
fn recognizes_size_diagnostics_without_classifying_transient_failures() {
    // Arrange
    let diagnostics = [
        "Input exceeds the maximum length of 1048576 characters.",
        "Codex app-server failed, then retry failed after restart: first error: Input exceeds the \
         maximum length of 1048576 characters.; retry error: Input exceeds the maximum length of \
         1048576 characters.",
        "contextWindowExceeded",
        "context_window_exceeded",
        "context window exceeded",
        "Maximum context length is 8192 tokens",
        "Prompt is too long",
    ];

    // Act / Assert
    for diagnostic in diagnostics {
        assert!(is_input_size_error(diagnostic), "{diagnostic}");
    }
    for diagnostic in ["network timeout", "429 rate limit", "permission denied", ""] {
        assert!(!is_input_size_error(diagnostic), "{diagnostic}");
    }
}
