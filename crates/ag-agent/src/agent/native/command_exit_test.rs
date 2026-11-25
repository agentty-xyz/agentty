use ag_contracts::{ActivityFacts, ActivityFailure, ActivityStatus};
use ag_harness::bash::{CommandExit, CommandTermination};

use super::apply_command_exit;

/// Applies `exit` to a completed row and returns the row's state.
fn applied(exit: &CommandExit) -> (ActivityStatus, Option<i64>, ActivityFacts) {
    let mut status = ActivityStatus::Completed;
    let mut exit_code = None;
    let mut facts = ActivityFacts::default();
    apply_command_exit(&mut status, &mut exit_code, &mut facts, exit);

    (status, exit_code, facts)
}

#[test]
fn completed_commands_keep_their_state_and_carry_their_exit() {
    // Arrange
    let exit = CommandExit::new(Some(0), None).with_pipe_status(vec![101, 0]);

    // Act
    let (status, exit_code, facts) = applied(&exit);

    // Assert
    assert_eq!(status, ActivityStatus::Completed);
    assert_eq!(exit_code, Some(0));
    assert_eq!(facts.pipe_status, [101, 0]);
    assert_eq!(facts.signal, None);
    assert_eq!(facts.failure, None);
}

#[test]
fn commands_that_did_not_run_to_their_end_are_judged_by_their_termination() {
    // Arrange
    let exit = |termination| CommandExit::new(None, Some(9)).with_termination(termination);

    // Act
    let timed_out = applied(&exit(CommandTermination::Deadline));
    let failed =
        applied(&CommandExit::new(None, None).with_termination(CommandTermination::Failed));
    let cancelled = applied(&exit(CommandTermination::Cancelled));

    // Assert
    assert_eq!(timed_out.0, ActivityStatus::Failed);
    assert_eq!(timed_out.2.failure, Some(ActivityFailure::TimedOut));
    assert_eq!(timed_out.2.reason.as_deref(), Some("timed out, signal 9"));
    assert_eq!(timed_out.2.signal, Some(9));
    assert_eq!(failed.0, ActivityStatus::Failed);
    assert_eq!(failed.2.failure, Some(ActivityFailure::Error));
    assert_eq!(failed.2.reason.as_deref(), Some("failed to run"));
    assert_eq!(cancelled.0, ActivityStatus::Interrupted);
    assert_eq!(cancelled.2.failure, None);
    assert_eq!(cancelled.2.reason, None);
}
