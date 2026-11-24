use std::time::SystemTime;

use super::{ActivityEvent, ActivityFacts, ActivityKind, ActivityStatus, LineRange};

/// Returns a command snapshot in `status` with `exit_code` and `facts`.
fn command(status: ActivityStatus, exit_code: Option<i64>, facts: ActivityFacts) -> ActivityEvent {
    ActivityEvent {
        attempt_id: "attempt".to_string(),
        exit_code,
        facts: Box::new(facts),
        id: "call".to_string(),
        input: None,
        kind: ActivityKind::Command,
        name: "bash".to_string(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: None,
        parent_id: None,
        status,
    }
}

#[test]
fn outcome_keeps_a_completed_command_that_exited_zero() {
    // Arrange
    let event = command(ActivityStatus::Completed, Some(0), ActivityFacts::default());

    // Act
    let outcome = event.outcome();

    // Assert
    assert_eq!(outcome, ActivityStatus::Completed);
    assert_eq!(event.effective_exit_code(), Some(0));
}

#[test]
fn outcome_fails_a_completed_command_with_a_nonzero_exit_code() {
    // Arrange
    let event = command(
        ActivityStatus::Completed,
        Some(101),
        ActivityFacts::default(),
    );

    // Act
    let outcome = event.outcome();

    // Assert
    assert_eq!(outcome, ActivityStatus::Failed);
}

#[test]
fn outcome_judges_a_pipeline_by_its_first_failing_stage() {
    // Arrange
    let facts = ActivityFacts {
        pipe_status: vec![0, 101, 0],
        ..ActivityFacts::default()
    };
    let event = command(ActivityStatus::Completed, Some(0), facts);

    // Act
    let outcome = event.outcome();

    // Assert
    assert_eq!(outcome, ActivityStatus::Failed);
    assert_eq!(event.effective_exit_code(), Some(101));
}

#[test]
fn outcome_falls_back_to_the_exit_code_when_every_stage_succeeded() {
    // Arrange
    let facts = ActivityFacts {
        pipe_status: vec![0, 0],
        ..ActivityFacts::default()
    };
    let event = command(ActivityStatus::Completed, Some(2), facts);

    // Act
    let effective_exit_code = event.effective_exit_code();

    // Assert
    assert_eq!(effective_exit_code, Some(2));
    assert_eq!(event.outcome(), ActivityStatus::Failed);
}

#[test]
fn outcome_fails_a_command_a_signal_terminated() {
    // Arrange
    let facts = ActivityFacts {
        signal: Some(9),
        ..ActivityFacts::default()
    };
    let event = command(ActivityStatus::Completed, None, facts);

    // Act
    let outcome = event.outcome();

    // Assert
    assert_eq!(outcome, ActivityStatus::Failed);
}

#[test]
fn outcome_keeps_provider_states_other_than_completed() {
    // Arrange
    let running = command(ActivityStatus::Running, Some(1), ActivityFacts::default());
    let interrupted = command(
        ActivityStatus::Interrupted,
        Some(1),
        ActivityFacts::default(),
    );
    let failed = command(ActivityStatus::Failed, Some(0), ActivityFacts::default());

    // Act
    let outcomes = [running.outcome(), interrupted.outcome(), failed.outcome()];

    // Assert
    assert_eq!(
        outcomes,
        [
            ActivityStatus::Running,
            ActivityStatus::Interrupted,
            ActivityStatus::Failed
        ]
    );
}

#[test]
fn bounded_line_keeps_the_first_nonblank_line_within_the_limit() {
    // Arrange
    let long = format!(
        "  \n {}\u{7}x\nsecond",
        "a".repeat(ActivityFacts::TEXT_LIMIT)
    );

    // Act
    let short = ActivityFacts::bounded_line("\n  hunk 1 context mismatch \nmore");
    let bounded = ActivityFacts::bounded_line(&long);
    let blank = ActivityFacts::bounded_line(" \n\t");

    // Assert
    assert_eq!(short.as_deref(), Some("hunk 1 context mismatch"));
    assert_eq!(
        bounded,
        Some(format!("{}…", "a".repeat(ActivityFacts::TEXT_LIMIT)))
    );
    assert_eq!(blank, None);
}

#[test]
fn from_window_covers_the_lines_a_read_selects() {
    // Arrange
    let windows = [
        (Some(1129), Some(100)),
        (None, Some(20)),
        (Some(7), None),
        (None, None),
        (Some(0), Some(0)),
    ];

    // Act
    let ranges = windows.map(|(offset, limit)| LineRange::from_window(offset, limit));

    // Assert
    assert_eq!(
        ranges,
        [
            Some(LineRange {
                first: 1129,
                last: Some(1228)
            }),
            Some(LineRange {
                first: 1,
                last: Some(20)
            }),
            Some(LineRange {
                first: 7,
                last: None
            }),
            None,
            None,
        ]
    );
}

#[test]
fn is_exploration_selects_reads_searches_and_listings() {
    // Arrange
    let kinds = [
        ActivityKind::Read,
        ActivityKind::Search,
        ActivityKind::List,
        ActivityKind::Command,
        ActivityKind::Tool,
    ];

    // Act
    let exploration = kinds.map(ActivityKind::is_exploration);

    // Assert
    assert_eq!(exploration, [true, true, true, false, false]);
}
