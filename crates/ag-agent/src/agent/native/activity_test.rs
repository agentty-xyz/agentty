use ag_contracts::{ActivityKind, ActivityStatus};
use ag_harness::lifecycle::ToolErrorType;

use super::{activity_kind, failure_status};

#[test]
fn activity_kind_classifies_built_in_tools() {
    // Arrange
    let names = ["bash", "write", "read"];

    // Act
    let kinds = names.map(activity_kind);

    // Assert
    assert_eq!(
        kinds,
        [
            ActivityKind::Command,
            ActivityKind::FileChange,
            ActivityKind::Tool
        ]
    );
}

#[test]
fn failure_status_interrupts_cancelled_calls_and_fails_the_rest() {
    // Arrange
    let error_types = [
        ToolErrorType::Cancelled,
        ToolErrorType::Execution,
        ToolErrorType::ApprovalDenied,
        ToolErrorType::ApprovalTimeout,
    ];

    // Act
    let statuses = error_types.map(failure_status);

    // Assert
    assert_eq!(
        statuses,
        [
            ActivityStatus::Interrupted,
            ActivityStatus::Failed,
            ActivityStatus::Failed,
            ActivityStatus::Failed
        ]
    );
}
