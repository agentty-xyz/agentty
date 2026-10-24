//! Public contract for recognizing provider input-size diagnostics.

use ag_contracts::{ProviderCallBudget, is_input_size_error};

#[test]
fn reservations_redeem_once_and_retries_charge_the_shared_pool() {
    // Arrange
    let budget = ProviderCallBudget::new(2);
    let reserved = budget.reserve().expect("reserve initial attempt");
    let retry = reserved.clone();
    assert_eq!(reserved.remaining(), 2);
    assert_eq!(budget.remaining(), 1);

    // Act / Assert
    reserved.consume().expect("redeem initial attempt");
    assert_eq!(budget.remaining(), 1, "no double charge");
    retry.consume().expect("charge retry");
    assert_eq!(budget.remaining(), 0);
    let error = retry.consume().expect_err("shared limit");
    assert!(is_input_size_error(&error.to_string()));
    drop(reserved);
    drop(retry);
    assert_eq!(budget.remaining(), 0, "spent attempts are never refunded");
}

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

#[test]
fn budget_clones_share_attempt_limits_and_preserve_input_size_classification() {
    // Arrange
    let budget = ag_contracts::ProviderCallBudget::new(1);
    let child = budget.clone();
    // Act
    child.consume().expect("first provider attempt");
    let error = budget.ensure_available().expect_err("shared limit");
    // Assert
    assert!(is_input_size_error(&error.to_string()));
    assert!(budget.consume().is_err());
    assert_eq!(budget.remaining(), 0);
}
