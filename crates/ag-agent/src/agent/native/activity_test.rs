use std::time::Duration;

use ag_contracts::{ActivityFailure, ActivityStatus};
use ag_harness::bash::CommandExit;
use ag_harness::lifecycle::ToolErrorType;

use super::ToolUpdate;

#[test]
fn completed_updates_carry_the_duration_and_a_command_exit() {
    // Arrange
    let exit = CommandExit::new(Some(0), None).with_pipe_status(vec![101, 0]);

    // Act
    let command = ToolUpdate::completed(Duration::from_secs(2), Some(&exit));
    let read = ToolUpdate::completed(Duration::from_millis(5), None);

    // Assert
    assert_eq!(command.status, ActivityStatus::Completed);
    assert_eq!(command.exit_code, Some(0));
    assert_eq!(command.facts.pipe_status, [101, 0]);
    assert_eq!(command.facts.signal, None);
    assert_eq!(command.facts.duration, Some(Duration::from_secs(2)));
    assert_eq!(read.exit_code, None);
    assert_eq!(read.facts.duration, Some(Duration::from_millis(5)));
}

#[test]
fn failed_updates_interrupt_cancelled_calls_and_name_other_failures() {
    // Arrange
    let error_types = [
        ToolErrorType::Cancelled,
        ToolErrorType::Execution,
        ToolErrorType::ApprovalDenied,
        ToolErrorType::ApprovalTimeout,
    ];

    // Act
    let updates = error_types.map(|error_type| {
        let update = ToolUpdate::failed(error_type);

        (update.status, update.facts.failure)
    });

    // Assert
    assert_eq!(
        updates,
        [
            (ActivityStatus::Interrupted, None),
            (ActivityStatus::Failed, Some(ActivityFailure::Error)),
            (ActivityStatus::Failed, Some(ActivityFailure::Denied)),
            (ActivityStatus::Failed, Some(ActivityFailure::TimedOut)),
        ]
    );
}
