//! Regression coverage for the manual benchmark's process result,
//! deterministic context fixtures, context metrics, and run traces.

#[path = "benchmark/context/fixture.rs"]
mod fixture;
#[path = "benchmark/context/fixture_test.rs"]
mod fixture_test;
#[path = "benchmark/context/metric.rs"]
mod metric;
#[path = "benchmark/context/metric_test.rs"]
mod metric_test;
#[path = "benchmark/context/probe.rs"]
mod probe;
#[cfg(test)]
#[path = "support/repository.rs"]
mod repository_fixture;
#[path = "benchmark/summary.rs"]
mod summary;
#[path = "benchmark/context/trace.rs"]
mod trace;
#[cfg(test)]
#[path = "benchmark/context/trace_test.rs"]
mod trace_test;

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
