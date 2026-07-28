//! Durable session-operation lifecycle types and transition rules.

use std::fmt;
use std::str::FromStr;

/// The kind of durable command executed by a session worker.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperationKind {
    /// Reads provider account metadata without changing a session.
    AccountRead,
    /// Rebases a session worktree onto its base branch.
    Rebase,
    /// Resumes an existing provider conversation.
    Reply,
    /// Starts a new provider conversation.
    StartPrompt,
    /// Runs a provider utility prompt inside an existing conversation.
    UtilityPrompt,
}

impl OperationKind {
    /// All operation kinds persisted by the session worker.
    pub const ALL: [Self; 5] = [
        Self::AccountRead,
        Self::Rebase,
        Self::Reply,
        Self::StartPrompt,
        Self::UtilityPrompt,
    ];

    /// Returns the canonical persisted representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccountRead => "account_read",
            Self::Rebase => "rebase",
            Self::Reply => "reply",
            Self::StartPrompt => "start_prompt",
            Self::UtilityPrompt => "utility_prompt",
        }
    }
}

impl fmt::Display for OperationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for OperationKind {
    type Err = ParseOperationKindError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "account_read" => Ok(Self::AccountRead),
            "rebase" => Ok(Self::Rebase),
            "reply" => Ok(Self::Reply),
            "start_prompt" => Ok(Self::StartPrompt),
            "utility_prompt" => Ok(Self::UtilityPrompt),
            _ => Err(ParseOperationKindError(value.to_string())),
        }
    }
}

/// Error returned when a persisted operation kind is unknown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseOperationKindError(String);

impl fmt::Display for ParseOperationKindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown operation kind `{}`", self.0)
    }
}

impl std::error::Error for ParseOperationKindError {}

/// The terminal result of a completed operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperationTerminalOutcome {
    /// The operation was canceled before successful completion.
    Canceled,
    /// The operation failed with a diagnostic.
    Failed,
    /// The operation completed successfully.
    Succeeded,
}

/// One typed lifecycle event applied to a durable operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperationTransition {
    /// A queued operation was sent to its worker queue.
    Dispatch,
    /// A worker received a dispatched operation.
    Claim,
    /// A claimed operation began executing.
    Start,
    /// Process shutdown interrupted an active operation.
    Interrupt,
    /// An active operation reached a terminal result.
    Finish(OperationTerminalOutcome),
}

impl OperationTransition {
    /// Every transition event used by the operation state machine.
    pub const ALL: [Self; 7] = [
        Self::Dispatch,
        Self::Claim,
        Self::Start,
        Self::Interrupt,
        Self::Finish(OperationTerminalOutcome::Canceled),
        Self::Finish(OperationTerminalOutcome::Failed),
        Self::Finish(OperationTerminalOutcome::Succeeded),
    ];
}

/// The durable lifecycle state of a session operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperationStatus {
    /// Persisted before delivery to a worker queue.
    Queued,
    /// Delivered to the worker queue but not received by the worker.
    Dispatched,
    /// Received by a worker but not yet executing.
    Claimed,
    /// Currently executing.
    Running,
    /// Left unfinished by process shutdown.
    Interrupted,
    /// Completed successfully.
    Succeeded,
    /// Failed with a diagnostic.
    Failed,
    /// Canceled before successful completion.
    Canceled,
}

impl OperationStatus {
    /// Every persisted operation status.
    pub const ALL: [Self; 8] = [
        Self::Queued,
        Self::Dispatched,
        Self::Claimed,
        Self::Running,
        Self::Interrupted,
        Self::Succeeded,
        Self::Failed,
        Self::Canceled,
    ];

    /// Returns the canonical persisted representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatched => "dispatched",
            Self::Claimed => "claimed",
            Self::Running => "running",
            Self::Interrupted => "interrupted",
            Self::Succeeded => "done",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    /// Returns whether this operation still requires execution or recovery.
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Dispatched | Self::Claimed | Self::Running
        )
    }

    /// Applies one lifecycle event through the canonical transition table.
    pub const fn transition(self, transition: OperationTransition) -> OperationTransitionOutcome {
        let next_status = match (self, transition) {
            (Self::Queued, OperationTransition::Dispatch) => Some(Self::Dispatched),
            (Self::Dispatched, OperationTransition::Claim) => Some(Self::Claimed),
            (Self::Claimed, OperationTransition::Start) => Some(Self::Running),
            (
                Self::Queued | Self::Dispatched | Self::Claimed | Self::Running,
                OperationTransition::Interrupt,
            ) => Some(Self::Interrupted),
            (
                Self::Queued | Self::Dispatched | Self::Claimed | Self::Running,
                OperationTransition::Finish(OperationTerminalOutcome::Canceled),
            ) => Some(Self::Canceled),
            (
                Self::Queued | Self::Dispatched | Self::Claimed | Self::Running,
                OperationTransition::Finish(OperationTerminalOutcome::Failed),
            ) => Some(Self::Failed),
            (Self::Running, OperationTransition::Finish(OperationTerminalOutcome::Succeeded)) => {
                Some(Self::Succeeded)
            }
            _ => None,
        };

        match next_status {
            Some(status) => OperationTransitionOutcome::Applied(status),
            None => OperationTransitionOutcome::Rejected {
                from: self,
                transition,
            },
        }
    }
}

impl fmt::Display for OperationStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for OperationStatus {
    type Err = ParseOperationStatusError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "dispatched" => Ok(Self::Dispatched),
            "claimed" => Ok(Self::Claimed),
            "running" => Ok(Self::Running),
            "interrupted" => Ok(Self::Interrupted),
            "done" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            _ => Err(ParseOperationStatusError(value.to_string())),
        }
    }
}

/// Error returned when a persisted operation status is unknown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseOperationStatusError(String);

impl fmt::Display for ParseOperationStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown operation status `{}`", self.0)
    }
}

impl std::error::Error for ParseOperationStatusError {}

/// Result of applying an operation lifecycle event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationTransitionOutcome {
    /// The transition was accepted and produced the contained status.
    Applied(OperationStatus),
    /// The transition was not valid from the current status.
    Rejected {
        /// Status from which the transition was attempted.
        from: OperationStatus,
        /// Lifecycle event rejected by the transition table.
        transition: OperationTransition,
    },
    /// No operation exists for the requested identifier.
    Missing {
        /// Lifecycle event that could not be applied.
        transition: OperationTransition,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_kind_round_trips_every_persisted_value() {
        // Arrange / Act / Assert
        for kind in OperationKind::ALL {
            assert_eq!(kind.as_str().parse(), Ok(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
    }

    #[test]
    fn test_operation_kind_rejects_unknown_persisted_value() {
        // Arrange / Act
        let error = "unknown".parse::<OperationKind>();

        // Assert
        assert_eq!(error, Err(ParseOperationKindError("unknown".to_string())));
        assert_eq!(
            error.expect_err("unknown kind should fail").to_string(),
            "unknown operation kind `unknown`"
        );
    }

    #[test]
    fn test_operation_status_round_trips_every_persisted_value() {
        // Arrange / Act / Assert
        for status in OperationStatus::ALL {
            assert_eq!(status.as_str().parse(), Ok(status));
            assert_eq!(status.to_string(), status.as_str());
        }
    }

    #[test]
    fn test_operation_status_rejects_unknown_persisted_value() {
        // Arrange / Act
        let error = "unknown".parse::<OperationStatus>();

        // Assert
        assert_eq!(error, Err(ParseOperationStatusError("unknown".to_string())));
        assert_eq!(
            error.expect_err("unknown status should fail").to_string(),
            "unknown operation status `unknown`"
        );
    }

    #[test]
    fn test_operation_status_classifies_every_active_and_terminal_state() {
        // Arrange
        let expected_active = [true, true, true, true, false, false, false, false];

        // Act
        let actual_active = OperationStatus::ALL.map(OperationStatus::is_active);

        // Assert
        assert_eq!(actual_active, expected_active);
    }

    #[test]
    fn test_operation_transition_table_covers_every_allowed_and_rejected_edge() {
        // Arrange / Act / Assert
        for status in OperationStatus::ALL {
            for transition in OperationTransition::ALL {
                let expected_status = expected_transition(status, transition);
                let expected_outcome = expected_status.map_or(
                    OperationTransitionOutcome::Rejected {
                        from: status,
                        transition,
                    },
                    OperationTransitionOutcome::Applied,
                );

                assert_eq!(status.transition(transition), expected_outcome);
            }
        }
    }

    fn expected_transition(
        status: OperationStatus,
        transition: OperationTransition,
    ) -> Option<OperationStatus> {
        match (status, transition) {
            (OperationStatus::Queued, OperationTransition::Dispatch) => {
                Some(OperationStatus::Dispatched)
            }
            (OperationStatus::Dispatched, OperationTransition::Claim) => {
                Some(OperationStatus::Claimed)
            }
            (OperationStatus::Claimed, OperationTransition::Start) => {
                Some(OperationStatus::Running)
            }
            (
                OperationStatus::Queued
                | OperationStatus::Dispatched
                | OperationStatus::Claimed
                | OperationStatus::Running,
                OperationTransition::Interrupt,
            ) => Some(OperationStatus::Interrupted),
            (
                OperationStatus::Queued
                | OperationStatus::Dispatched
                | OperationStatus::Claimed
                | OperationStatus::Running,
                OperationTransition::Finish(OperationTerminalOutcome::Canceled),
            ) => Some(OperationStatus::Canceled),
            (
                OperationStatus::Queued
                | OperationStatus::Dispatched
                | OperationStatus::Claimed
                | OperationStatus::Running,
                OperationTransition::Finish(OperationTerminalOutcome::Failed),
            ) => Some(OperationStatus::Failed),
            (
                OperationStatus::Running,
                OperationTransition::Finish(OperationTerminalOutcome::Succeeded),
            ) => Some(OperationStatus::Succeeded),
            _ => None,
        }
    }
}
