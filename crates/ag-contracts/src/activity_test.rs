use std::time::SystemTime;

use super::{ActivityEvent, ActivityFacts, ActivityKind, ActivityStatus};

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
fn outcome_skips_upstream_stages_that_a_closed_pipe_ended() {
    // Arrange
    let head = ActivityFacts {
        pipe_status: vec![141, 0],
        ..ActivityFacts::default()
    };
    let last_stage = ActivityFacts {
        pipe_status: vec![0, 141],
        ..ActivityFacts::default()
    };
    let behind_a_failure = ActivityFacts {
        pipe_status: vec![141, 2, 0],
        ..ActivityFacts::default()
    };

    // Act
    let events = [
        command(ActivityStatus::Completed, Some(0), head),
        command(ActivityStatus::Completed, Some(141), last_stage),
        command(ActivityStatus::Completed, Some(0), behind_a_failure),
    ];

    // Assert
    assert_eq!(events[0].effective_exit_code(), Some(0));
    assert_eq!(events[0].outcome(), ActivityStatus::Completed);
    assert_eq!(events[1].effective_exit_code(), Some(141));
    assert_eq!(events[1].outcome(), ActivityStatus::Failed);
    assert_eq!(events[2].effective_exit_code(), Some(2));
}

#[test]
fn outcome_keeps_a_search_that_found_nothing_completed() {
    // Arrange
    let mut search = command(ActivityStatus::Completed, Some(1), ActivityFacts::default());
    search.kind = ActivityKind::Search;
    let mut broken_search = search.clone();
    broken_search.exit_code = Some(2);
    let command_search = command(ActivityStatus::Completed, Some(1), ActivityFacts::default());

    // Act
    let outcomes = [
        search.outcome(),
        broken_search.outcome(),
        command_search.outcome(),
    ];

    // Assert
    assert_eq!(
        outcomes,
        [
            ActivityStatus::Completed,
            ActivityStatus::Failed,
            ActivityStatus::Failed
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
