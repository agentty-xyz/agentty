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

#[test]
fn redacts_provider_response_bodies_from_benchmark_details() {
    // Arrange
    let detail = "model request failed: Kimi returned HTTP 429 Too Many Requests: secret body";

    // Act
    let sanitized = summary::sanitize_detail(detail);

    // Assert
    assert_eq!(
        sanitized,
        "model request failed: Kimi returned HTTP 429 Too Many Requests: <redacted>"
    );
}

#[test]
fn normalizes_non_provider_benchmark_details() {
    // Arrange
    let detail = "schema failed\nwithout provider response";

    // Act
    let sanitized = summary::sanitize_detail(detail);

    // Assert
    assert_eq!(sanitized, "schema failed without provider response");
}
