//! Retry policy tests for retried kinds, budgets, backoff, jitter, and
//! `Retry-After` parsing.

use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

use super::{Jitter, RetryKind, RetryPolicy};

/// Fixed wall-clock time HTTP dates are read against.
fn now() -> SystemTime {
    httpdate::parse_http_date("Wed, 21 Oct 2026 07:28:00 GMT").expect("fixture date parses")
}

/// Returns headers carrying `Retry-After: value`.
fn retry_after_headers(value: &[u8]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        RETRY_AFTER,
        HeaderValue::from_bytes(value).expect("valid header bytes"),
    );

    headers
}

#[test]
fn classifies_retryable_statuses() {
    // Arrange
    let cases = [
        (429, Some(RetryKind::RateLimited)),
        (500, Some(RetryKind::ServerError)),
        (502, Some(RetryKind::ServerError)),
        (503, Some(RetryKind::ServerError)),
        (504, Some(RetryKind::ServerError)),
        (529, Some(RetryKind::Overloaded)),
        (400, None),
        (401, None),
        (402, None),
        (501, None),
    ];

    for (status, expected) in cases {
        // Act
        let kind = RetryKind::of_status(StatusCode::from_u16(status).expect("valid status"));

        // Assert
        assert_eq!(kind, expected, "{status}");
    }
}

#[test]
fn default_policy_retries_rate_limits_and_one_connection_failure() {
    // Arrange
    let policy = RetryPolicy::default();

    // Act / Assert
    assert!(policy.allows(RetryKind::RateLimited, 4, 0));
    assert!(!policy.allows(RetryKind::RateLimited, 5, 0));
    assert!(!policy.allows(RetryKind::ServerError, 0, 0));
    assert!(!policy.allows(RetryKind::Overloaded, 0, 0));
    assert!(policy.allows(RetryKind::Connection, 5, 0));
    assert!(!policy.allows(RetryKind::Connection, 0, 1));
}

#[test]
fn opt_ins_and_budgets_select_what_is_retried() {
    // Arrange
    let policy = RetryPolicy::default()
        .with_retried(RetryKind::ServerError, true)
        .with_retried(RetryKind::Overloaded, true)
        .with_max_attempts(3)
        .with_max_connection_retries(2);
    let none = RetryPolicy::none();
    let single = RetryPolicy::default().with_max_attempts(0);

    // Act / Assert
    assert!(policy.allows(RetryKind::ServerError, 1, 0));
    assert!(policy.allows(RetryKind::Overloaded, 1, 0));
    assert!(!policy.allows(RetryKind::Overloaded, 2, 0));
    assert!(policy.allows(RetryKind::Connection, 0, 1));
    assert!(!policy.allows(RetryKind::Connection, 0, 2));
    for kind in [
        RetryKind::RateLimited,
        RetryKind::ServerError,
        RetryKind::Overloaded,
        RetryKind::Connection,
    ] {
        assert!(!none.allows(kind, 0, 0), "{kind:?}");
    }
    assert!(!single.allows(RetryKind::RateLimited, 0, 0));
}

#[test]
fn status_delay_doubles_to_its_cap_or_waits_the_longer_retry_after() {
    // Arrange
    let policy = RetryPolicy::default();
    let cases = [
        (0, None, Duration::from_secs(1)),
        (1, None, Duration::from_secs(2)),
        (2, Some(Duration::from_secs(1)), Duration::from_secs(4)),
        (0, Some(Duration::from_secs(30)), Duration::from_secs(30)),
        (9, None, Duration::from_secs(5)),
        (40, None, Duration::from_secs(5)),
    ];

    for (retry, retry_after, expected) in cases {
        // Act
        let delay = policy.status_delay(retry, retry_after, 0.5);

        // Assert
        assert_eq!(delay, expected, "{retry} {retry_after:?}");
    }
}

#[test]
fn jitter_spreads_the_backoff_but_never_a_retry_after() {
    // Arrange
    let full = RetryPolicy::default()
        .with_base_delay(Duration::from_secs(4))
        .with_max_backoff(Duration::from_secs(8))
        .with_jitter(Jitter::Full);
    let equal = full.with_jitter(Jitter::Equal);

    // Act / Assert
    assert_eq!(full.status_delay(0, None, 0.25), Duration::from_secs(1));
    assert_eq!(full.status_delay(1, None, 0.0), Duration::ZERO);
    assert_eq!(
        full.status_delay(0, Some(Duration::from_secs(3)), 0.0),
        Duration::from_secs(3)
    );
    assert_eq!(equal.status_delay(0, None, 0.5), Duration::from_secs(3));
    assert_eq!(equal.status_delay(0, None, 2.0), Duration::from_secs(4));
    assert_eq!(full.connection_delay(0.5), Duration::from_secs(2));
    assert_eq!(
        RetryPolicy::default().connection_delay(0.9),
        Duration::from_secs(1)
    );
}

#[test]
fn retry_after_reads_capped_delta_seconds_and_http_dates() {
    // Arrange
    let policy = RetryPolicy::default();
    let cases: [(&[u8], Option<Duration>); 9] = [
        (b"7", Some(Duration::from_secs(7))),
        (b" 0 ", Some(Duration::ZERO)),
        (b"18446744073709551615", Some(Duration::from_mins(1))),
        (
            b"Wed, 21 Oct 2026 07:28:45 GMT",
            Some(Duration::from_secs(45)),
        ),
        (
            b"Wednesday, 21-Oct-26 07:28:30 GMT",
            Some(Duration::from_secs(30)),
        ),
        (
            b"Wed, 21 Oct 2026 09:00:00 GMT",
            Some(Duration::from_mins(1)),
        ),
        (b"Wed, 21 Oct 2015 07:28:00 GMT", Some(Duration::ZERO)),
        (b"soon", None),
        (b"\xff", None),
    ];

    for (value, expected) in cases {
        // Act
        let delay = policy.retry_after(&retry_after_headers(value), now());

        // Assert
        assert_eq!(delay, expected, "{value:?}");
    }
    assert_eq!(policy.retry_after(&HeaderMap::new(), now()), None);
    assert_eq!(
        policy
            .with_max_retry_after(Duration::from_secs(10))
            .retry_after(&retry_after_headers(b"30"), now()),
        Some(Duration::from_secs(10))
    );
}
