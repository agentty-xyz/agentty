//! Persistence codecs for lifecycle status strings.

use std::str::FromStr;

use ag_session::{OrchestrationStatus, OrchestrationTaskStatus, SessionStatus};

use crate::DbError;

/// Validates one persisted session-operation status.
pub(super) fn validate_operation(value: &str) -> Result<(), DbError> {
    OperationStatus::from_str(value).map(|_| ())
}

/// Validates one persisted orchestration status.
pub(super) fn validate_orchestration(value: &str) -> Result<(), DbError> {
    validate::<OrchestrationStatus>("orchestration", value)
}

/// Validates one persisted orchestration-task status.
pub(super) fn validate_orchestration_task(value: &str) -> Result<(), DbError> {
    validate::<OrchestrationTaskStatus>("orchestration task", value)
}

/// Validates one persisted session status, including supported legacy values.
pub(super) fn validate_session(value: &str) -> Result<(), DbError> {
    validate::<SessionStatus>("session", value)
}

/// Valid operation statuses stored by the persistence adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationStatus {
    Canceled,
    Done,
    Failed,
    Queued,
    Running,
}

impl FromStr for OperationStatus {
    type Err = DbError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "canceled" => Ok(Self::Canceled),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            _ => Err(invalid_status("session operation", value)),
        }
    }
}

/// Parses one domain-owned status without leaking its parser error shape.
fn validate<Status>(entity: &'static str, value: &str) -> Result<(), DbError>
where
    Status: FromStr,
{
    Status::from_str(value)
        .map(|_| ())
        .map_err(|_| invalid_status(entity, value))
}

/// Builds a consistent invalid-status error for every persistence codec.
fn invalid_status(entity: &'static str, value: &str) -> DbError {
    DbError::InvalidStatus {
        entity,
        value: value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codecs_accept_current_and_legacy_statuses() {
        // Arrange / Act / Assert
        assert!(validate_operation("queued").is_ok());
        assert!(validate_orchestration("Running").is_ok());
        assert!(validate_orchestration_task("WaitingForInput").is_ok());
        assert!(validate_session("Committing").is_ok());
    }

    #[test]
    fn codecs_reject_unknown_statuses_with_entity_context() {
        // Arrange
        let statuses = [
            validate_operation("unknown"),
            validate_orchestration("unknown"),
            validate_orchestration_task("unknown"),
            validate_session("unknown"),
        ];

        // Act
        let messages =
            statuses.map(|result| result.expect_err("unknown status should fail").to_string());

        // Assert
        assert_eq!(
            messages,
            [
                "Invalid session operation lifecycle status `unknown`",
                "Invalid orchestration lifecycle status `unknown`",
                "Invalid orchestration task lifecycle status `unknown`",
                "Invalid session lifecycle status `unknown`",
            ]
        );
    }
}
