//! Orchestration and orchestration-task lifecycle states.
//!
//! One orchestration groups the child sessions proposed by a single controller
//! plan. The orchestration row tracks whether that plan is still awaiting the
//! user's approval, actively fanning out, or settled; each task row tracks one
//! child session through creation, execution, and settlement.

use std::fmt;
use std::str::FromStr;

/// Lifecycle state for one controller-owned orchestration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrchestrationStatus {
    /// The plan is persisted and parked on a clarification question.
    AwaitingApproval,
    /// The plan is approved and its tasks are fanning out.
    Running,
    /// Cancellation is blocking new fan-out while active children stop.
    Canceling,
    /// Every task settled and one durable roll-up delivery is being claimed.
    Submitting,
    /// Every task settled and the controller received its roll-up turn.
    Done,
    /// The user canceled the orchestration or its controller session.
    Canceled,
}

impl OrchestrationStatus {
    /// Returns whether the orchestration is still open.
    ///
    /// An open plan blocks another plan from being persisted for the same
    /// controller, including while it waits for approval.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            OrchestrationStatus::AwaitingApproval
                | OrchestrationStatus::Running
                | OrchestrationStatus::Canceling
                | OrchestrationStatus::Submitting
        )
    }
}

impl fmt::Display for OrchestrationStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            OrchestrationStatus::AwaitingApproval => "AwaitingApproval",
            OrchestrationStatus::Running => "Running",
            OrchestrationStatus::Canceling => "Canceling",
            OrchestrationStatus::Submitting => "Submitting",
            OrchestrationStatus::Done => "Done",
            OrchestrationStatus::Canceled => "Canceled",
        };

        formatter.write_str(value)
    }
}

impl FromStr for OrchestrationStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "AwaitingApproval" => Ok(OrchestrationStatus::AwaitingApproval),
            "Running" => Ok(OrchestrationStatus::Running),
            "Canceling" => Ok(OrchestrationStatus::Canceling),
            "Submitting" => Ok(OrchestrationStatus::Submitting),
            "Done" => Ok(OrchestrationStatus::Done),
            "Canceled" => Ok(OrchestrationStatus::Canceled),
            _ => Err(format!("Unknown orchestration status: {value}")),
        }
    }
}

/// Lifecycle state for one orchestration task and its child session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrchestrationTaskStatus {
    /// Persisted with the proposed plan, not yet approved or fanned out.
    Planned,
    /// The child session is being created and started.
    Creating,
    /// The child session is running its turn.
    Running,
    /// The child session parked on clarification questions.
    WaitingForInput,
    /// The child session finished and is ready for review or integration.
    Ready,
    /// The child session failed, or a straggler was canceled out of band.
    Failed,
    /// The task was canceled as part of a cascade cancel.
    Canceled,
}

impl OrchestrationTaskStatus {
    /// Returns whether the task reached a state that fan-in treats as settled.
    ///
    /// A canceled straggler counts as settled so out-of-band cancellation
    /// unblocks the roll-up instead of stalling it.
    pub fn is_settled(self) -> bool {
        matches!(
            self,
            OrchestrationTaskStatus::Ready
                | OrchestrationTaskStatus::Failed
                | OrchestrationTaskStatus::Canceled
        )
    }

    /// Returns whether the task currently occupies a parallelism slot.
    ///
    /// A task waiting for user input still holds its child session and
    /// worktree, so it keeps consuming a slot until the user answers.
    pub fn occupies_parallelism_slot(self) -> bool {
        matches!(
            self,
            OrchestrationTaskStatus::Creating
                | OrchestrationTaskStatus::Running
                | OrchestrationTaskStatus::WaitingForInput
        )
    }

    /// Returns whether a transition to `next` is valid.
    ///
    /// Retry re-enters `Creating` from a settled state with the same task key,
    /// which is what makes replying "retry the failed tasks" a clean respawn
    /// rather than a duplicate fan-out.
    pub fn can_transition_to(self, next: OrchestrationTaskStatus) -> bool {
        if self == next {
            return true;
        }

        matches!(
            (self, next),
            (
                OrchestrationTaskStatus::Planned
                    | OrchestrationTaskStatus::Ready
                    | OrchestrationTaskStatus::Failed
                    | OrchestrationTaskStatus::Canceled,
                OrchestrationTaskStatus::Creating
            ) | (
                OrchestrationTaskStatus::Creating | OrchestrationTaskStatus::WaitingForInput,
                OrchestrationTaskStatus::Running
            ) | (
                OrchestrationTaskStatus::Running,
                OrchestrationTaskStatus::WaitingForInput
            ) | (
                OrchestrationTaskStatus::Running | OrchestrationTaskStatus::WaitingForInput,
                OrchestrationTaskStatus::Ready
            ) | (
                OrchestrationTaskStatus::Planned
                    | OrchestrationTaskStatus::Creating
                    | OrchestrationTaskStatus::Running
                    | OrchestrationTaskStatus::WaitingForInput,
                OrchestrationTaskStatus::Failed | OrchestrationTaskStatus::Canceled
            )
        )
    }
}

impl fmt::Display for OrchestrationTaskStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            OrchestrationTaskStatus::Planned => "Planned",
            OrchestrationTaskStatus::Creating => "Creating",
            OrchestrationTaskStatus::Running => "Running",
            OrchestrationTaskStatus::WaitingForInput => "WaitingForInput",
            OrchestrationTaskStatus::Ready => "Ready",
            OrchestrationTaskStatus::Failed => "Failed",
            OrchestrationTaskStatus::Canceled => "Canceled",
        };

        formatter.write_str(value)
    }
}

impl FromStr for OrchestrationTaskStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Planned" => Ok(OrchestrationTaskStatus::Planned),
            "Creating" => Ok(OrchestrationTaskStatus::Creating),
            "Running" => Ok(OrchestrationTaskStatus::Running),
            "WaitingForInput" => Ok(OrchestrationTaskStatus::WaitingForInput),
            "Ready" => Ok(OrchestrationTaskStatus::Ready),
            "Failed" => Ok(OrchestrationTaskStatus::Failed),
            "Canceled" => Ok(OrchestrationTaskStatus::Canceled),
            _ => Err(format!("Unknown orchestration task status: {value}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Round-trips every orchestration status through its persisted form.
    fn test_orchestration_status_round_trips_persisted_values() {
        // Arrange
        let statuses = [
            OrchestrationStatus::AwaitingApproval,
            OrchestrationStatus::Running,
            OrchestrationStatus::Canceling,
            OrchestrationStatus::Submitting,
            OrchestrationStatus::Done,
            OrchestrationStatus::Canceled,
        ];

        // Act
        let round_tripped = statuses.map(|status| {
            status
                .to_string()
                .parse::<OrchestrationStatus>()
                .expect("status should parse")
        });

        // Assert
        assert_eq!(round_tripped, statuses);
        assert!("Unknown".parse::<OrchestrationStatus>().is_err());
    }

    #[test]
    /// Restricts restart re-linking to orchestrations that still need work.
    fn test_only_unsettled_orchestrations_are_active() {
        // Arrange / Act / Assert
        assert!(OrchestrationStatus::AwaitingApproval.is_active());
        assert!(OrchestrationStatus::Running.is_active());
        assert!(OrchestrationStatus::Canceling.is_active());
        assert!(OrchestrationStatus::Submitting.is_active());
        assert!(!OrchestrationStatus::Done.is_active());
        assert!(!OrchestrationStatus::Canceled.is_active());
    }

    #[test]
    /// Round-trips every task status through its persisted form.
    fn test_orchestration_task_status_round_trips_persisted_values() {
        // Arrange
        let statuses = [
            OrchestrationTaskStatus::Planned,
            OrchestrationTaskStatus::Creating,
            OrchestrationTaskStatus::Running,
            OrchestrationTaskStatus::WaitingForInput,
            OrchestrationTaskStatus::Ready,
            OrchestrationTaskStatus::Failed,
            OrchestrationTaskStatus::Canceled,
        ];

        // Act
        let round_tripped = statuses.map(|status| {
            status
                .to_string()
                .parse::<OrchestrationTaskStatus>()
                .expect("status should parse")
        });

        // Assert
        assert_eq!(round_tripped, statuses);
        assert!("Unknown".parse::<OrchestrationTaskStatus>().is_err());
    }

    #[test]
    /// Treats a canceled straggler as settled so fan-in is not blocked by
    /// out-of-band cancellation.
    fn test_settled_task_statuses_include_cancellation() {
        // Arrange / Act / Assert
        assert!(OrchestrationTaskStatus::Ready.is_settled());
        assert!(OrchestrationTaskStatus::Failed.is_settled());
        assert!(OrchestrationTaskStatus::Canceled.is_settled());
        assert!(!OrchestrationTaskStatus::Planned.is_settled());
        assert!(!OrchestrationTaskStatus::Creating.is_settled());
        assert!(!OrchestrationTaskStatus::Running.is_settled());
        assert!(!OrchestrationTaskStatus::WaitingForInput.is_settled());
    }

    #[test]
    /// Counts a task waiting for user input against the parallelism cap
    /// because it still owns a live child session and worktree.
    fn test_parallelism_slots_cover_every_live_child() {
        // Arrange / Act / Assert
        assert!(OrchestrationTaskStatus::Creating.occupies_parallelism_slot());
        assert!(OrchestrationTaskStatus::Running.occupies_parallelism_slot());
        assert!(OrchestrationTaskStatus::WaitingForInput.occupies_parallelism_slot());
        assert!(!OrchestrationTaskStatus::Planned.occupies_parallelism_slot());
        assert!(!OrchestrationTaskStatus::Ready.occupies_parallelism_slot());
        assert!(!OrchestrationTaskStatus::Failed.occupies_parallelism_slot());
        assert!(!OrchestrationTaskStatus::Canceled.occupies_parallelism_slot());
    }

    #[test]
    /// Allows the fan-out, question, settle, and retry transitions the
    /// coordinator drives, and rejects skipping creation.
    fn test_task_status_transitions_cover_fan_out_and_retry() {
        // Arrange / Act / Assert
        assert!(
            OrchestrationTaskStatus::Planned.can_transition_to(OrchestrationTaskStatus::Creating)
        );
        assert!(
            OrchestrationTaskStatus::Creating.can_transition_to(OrchestrationTaskStatus::Running)
        );
        assert!(
            OrchestrationTaskStatus::Running
                .can_transition_to(OrchestrationTaskStatus::WaitingForInput)
        );
        assert!(
            OrchestrationTaskStatus::WaitingForInput
                .can_transition_to(OrchestrationTaskStatus::Running)
        );
        assert!(OrchestrationTaskStatus::Running.can_transition_to(OrchestrationTaskStatus::Ready));
        assert!(
            OrchestrationTaskStatus::Running.can_transition_to(OrchestrationTaskStatus::Canceled)
        );
        assert!(
            OrchestrationTaskStatus::Failed.can_transition_to(OrchestrationTaskStatus::Creating)
        );
        assert!(OrchestrationTaskStatus::Ready.can_transition_to(OrchestrationTaskStatus::Ready));
        assert!(
            !OrchestrationTaskStatus::Planned.can_transition_to(OrchestrationTaskStatus::Running)
        );
        assert!(
            !OrchestrationTaskStatus::Canceled.can_transition_to(OrchestrationTaskStatus::Ready)
        );
    }
}
