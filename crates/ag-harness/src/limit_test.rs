//! Run-limit checks, deadlines, and limit names.

use std::time::Duration;

use serde_json::json;
use tokio::time::Instant;

use crate::limit::{LimitKind, RunLimits};

#[test]
fn default_limits_never_stop_a_run() {
    // Arrange
    let limits = RunLimits::default();

    // Act
    let request = limits.check_request(u64::MAX, u64::MAX);
    let tool_calls = limits.check_tool_calls(usize::MAX, usize::MAX);
    let deadline = limits.deadline();

    // Assert
    assert_eq!(request, Ok(()));
    assert_eq!(tool_calls, Ok(()));
    assert_eq!(deadline, None);
}

#[test]
fn request_checks_stop_at_rounds_before_tokens() {
    // Arrange
    let limits = RunLimits::default()
        .with_max_rounds(2)
        .with_max_total_tokens(100);

    // Act
    let results = [
        limits.check_request(1, 99),
        limits.check_request(2, 0),
        limits.check_request(1, 100),
        limits.check_request(2, 100),
    ];

    // Assert
    assert_eq!(
        results,
        [
            Ok(()),
            Err(LimitKind::Rounds),
            Err(LimitKind::Tokens),
            Err(LimitKind::Rounds),
        ]
    );
}

#[test]
fn tool_call_check_rejects_a_batch_that_would_exceed_the_limit() {
    // Arrange
    let limits = RunLimits::default().with_max_tool_calls(3);

    // Act
    let within = limits.check_tool_calls(1, 2);
    let beyond = limits.check_tool_calls(2, 2);
    let none_allowed = RunLimits::default()
        .with_max_tool_calls(0)
        .check_tool_calls(0, 1);

    // Assert
    assert_eq!(within, Ok(()));
    assert_eq!(beyond, Err(LimitKind::ToolCalls));
    assert_eq!(none_allowed, Err(LimitKind::ToolCalls));
}

#[tokio::test(start_paused = true)]
async fn deadline_follows_the_wall_time_unless_the_clock_cannot_hold_it() {
    // Arrange
    let limits = RunLimits::default().with_max_wall_time(Duration::from_secs(5));
    let unbounded = RunLimits::default().with_max_wall_time(Duration::MAX);

    // Act
    let deadline = limits.deadline();
    let overflowing = unbounded.deadline();

    // Assert
    assert_eq!(deadline, Some(Instant::now() + Duration::from_secs(5)));
    assert_eq!(overflowing, None);
}

#[test]
fn limit_kinds_have_stable_names() {
    // Arrange
    let kinds = [
        LimitKind::Rounds,
        LimitKind::ToolCalls,
        LimitKind::Tokens,
        LimitKind::WallTime,
    ];

    // Act
    let names = kinds.map(|kind| kind.to_string());
    let serialized = serde_json::to_value(kinds).expect("limit kinds serialize");

    // Assert
    assert_eq!(names, ["rounds", "tool_calls", "tokens", "wall_time"]);
    assert_eq!(serialized, json!(names));
}
