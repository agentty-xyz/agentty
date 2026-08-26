//! Regression coverage for the manual benchmark's process result.

#[path = "benchmark/summary.rs"]
mod summary;

#[test]
fn accepts_complete_benchmark_summary() {
    // Arrange
    let passed = 30;
    let total = 30;

    // Act
    let result = summary::ensure_all_passed(passed, total);

    // Assert
    assert_eq!(result, Ok(()));
}

#[test]
fn rejects_incomplete_benchmark_summary() {
    // Arrange
    let passed = 29;
    let total = 30;

    // Act
    let error = summary::ensure_all_passed(passed, total)
        .expect_err("failed benchmark cases should produce an error exit");

    // Assert
    assert_eq!(error.to_string(), "benchmark failed: 29 of 30 cases passed");
}
