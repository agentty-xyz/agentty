//! Run-limit checks, deadlines, and limit names.

use std::num::{NonZeroU32, NonZeroU64};
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
    let deadline = limits.deadline(Instant::now());

    // Assert
    assert_eq!(request, Ok(()));
    assert_eq!(tool_calls, Ok(()));
    assert_eq!(deadline, None);
    assert!(!limits.is_unsatisfiable());
}

#[test]
fn request_checks_stop_at_model_requests_before_tokens() {
    // Arrange
    let limits = RunLimits::default()
        .with_max_model_requests(NonZeroU32::MIN.saturating_add(1))
        .with_max_total_tokens(NonZeroU64::MIN.saturating_add(99));

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
            Err(LimitKind::ModelRequests),
            Err(LimitKind::Tokens),
            Err(LimitKind::ModelRequests),
        ]
    );
}

#[test]
fn tool_call_check_rejects_a_batch_that_would_exceed_the_limit() {
    // Arrange
    let limits = RunLimits::default().with_max_tool_calls(NonZeroU32::MIN.saturating_add(2));

    // Act
    let within = limits.check_tool_calls(1, 2);
    let beyond = limits.check_tool_calls(2, 2);

    // Assert
    assert_eq!(within, Ok(()));
    assert_eq!(beyond, Err(LimitKind::ToolCalls));
}

#[test]
fn only_a_zero_wall_time_is_unsatisfiable() {
    // Arrange
    let zero = RunLimits::default().with_max_wall_time(Duration::ZERO);
    let positive = RunLimits::default().with_max_wall_time(Duration::from_nanos(1));

    // Act
    let unsatisfiable = [zero.is_unsatisfiable(), positive.is_unsatisfiable()];

    // Assert
    assert_eq!(unsatisfiable, [true, false]);
}

#[test]
fn compaction_shares_leave_the_run_a_request_and_half_its_tokens() {
    // Arrange
    let requests = |max: u32| {
        RunLimits::default().with_max_model_requests(NonZeroU32::new(max).expect("nonzero"))
    };
    let tokens = |max: u64| {
        RunLimits::default().with_max_total_tokens(NonZeroU64::new(max).expect("nonzero"))
    };
    let wall_time = RunLimits::default().with_max_wall_time(Duration::from_secs(5));

    // Act
    let shares = [
        RunLimits::default().compaction_share(),
        requests(1).compaction_share(),
        requests(3).compaction_share(),
        tokens(1).compaction_share(),
        tokens(1_001).compaction_share(),
        wall_time.compaction_share(),
    ];

    // Assert
    assert_eq!(
        shares,
        [
            Some(RunLimits::default()),
            None,
            Some(requests(2)),
            None,
            Some(tokens(500)),
            Some(wall_time),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn deadline_follows_the_wall_time_unless_the_clock_cannot_hold_it() {
    // Arrange
    let limits = RunLimits::default().with_max_wall_time(Duration::from_secs(5));
    let unbounded = RunLimits::default().with_max_wall_time(Duration::MAX);

    // Act
    let deadline = limits.deadline(Instant::now());
    let overflowing = unbounded.deadline(Instant::now());

    // Assert
    assert_eq!(deadline, Some(Instant::now() + Duration::from_secs(5)));
    assert_eq!(overflowing, None);
}

#[test]
fn limit_kinds_have_stable_names() {
    // Arrange
    let kinds = [
        LimitKind::ModelRequests,
        LimitKind::ToolCalls,
        LimitKind::Tokens,
        LimitKind::WallTime,
        LimitKind::Cost,
    ];

    // Act
    let names = kinds.map(|kind| kind.to_string());
    let serialized = serde_json::to_value(kinds).expect("limit kinds serialize");

    // Assert
    assert_eq!(
        names,
        [
            "model_requests",
            "tool_calls",
            "tokens",
            "wall_time",
            "cost"
        ]
    );
    assert_eq!(serialized, json!(names));
}
