//! Failure classification unit tests proving every turn, session, and model
//! failure maps to one kind with the right retry signal.

use std::error::Error;
use std::io;
use std::time::Duration;

use serde_json::json;

use crate::bash::{BashError, CommandCleanupScope, CommandOutcome, CommandTermination};
use crate::error::{ErrorKind, HarnessError, RunError};
use crate::model::{ModelError, ModelErrorType, ModelRegistryError, ProviderStatus};
use crate::recovery::{HostRequest, HostTurnRecord, HostTurnStatus};
use crate::store::CheckpointError;
use crate::tool::{ReadError, WriteError};
use crate::{OutputSchemaError, SessionError, TurnError};

fn command_outcome() -> Box<CommandOutcome> {
    Box::new(CommandOutcome {
        cleanup_failed: false,
        cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
        execution_failure: None,
        exit_code: Some(1),
        signal: None,
        stderr: String::new(),
        stderr_omitted_bytes: 0,
        stderr_tail: String::new(),
        stdout: String::new(),
        stdout_omitted_bytes: 0,
        stdout_tail: String::new(),
        termination: CommandTermination::Completed,
        truncated: false,
    })
}

fn host_turn_record() -> Box<HostTurnRecord> {
    Box::new(HostTurnRecord {
        commands: Vec::new(),
        model: None,
        request: HostRequest::from_configuration("id".into(), json!({"request": "fixture"}))
            .expect("host request fixture should be valid"),
        status: HostTurnStatus::InProgress,
        turn_position: 0,
        writes: Vec::new(),
    })
}

fn provider_error(http_status: Option<u16>, quota_exceeded: bool) -> ModelError {
    ModelError::classified_request(
        ModelErrorType::Provider,
        ProviderStatus {
            http_status,
            quota_exceeded,
            retry_after: Some(Duration::from_secs(3)),
        },
        io::Error::other("provider rejected the request").into(),
    )
}

fn classified(error_type: ModelErrorType) -> ModelError {
    ModelError::classified_request(
        error_type,
        ProviderStatus::default(),
        io::Error::other("classified").into(),
    )
}

fn store_failure() -> SessionError {
    SessionError::Store {
        operation: "fixture",
        source: io::Error::other("store offline").into(),
    }
}

#[test]
fn classifies_every_turn_failure() {
    // Arrange
    let cases = [
        (
            TurnError::CommandFailed {
                outcome: command_outcome(),
            },
            ErrorKind::ToolFailed,
        ),
        (
            TurnError::CommandJournal {
                outcome: Some(command_outcome()),
                source: Box::new(store_failure()),
            },
            ErrorKind::Store,
        ),
        (
            TurnError::Bash(BashError::InvalidPolicy),
            ErrorKind::ToolFailed,
        ),
        (
            TurnError::MessageJournal {
                source: Box::new(store_failure()),
            },
            ErrorKind::Store,
        ),
        (TurnError::Cancelled, ErrorKind::Cancelled),
        (
            TurnError::Model(ModelError::InvalidResponse),
            ErrorKind::InvalidOutput,
        ),
        (
            TurnError::Read(ReadError::OutsideRepository {
                path: "../secret".to_string(),
            }),
            ErrorKind::ToolFailed,
        ),
        (TurnError::RepositoryRequired, ErrorKind::Config),
        (TurnError::ComparisonRepositoryMismatch, ErrorKind::Config),
        (
            TurnError::Write(WriteError::BinaryTarget {
                path: "image.png".to_string(),
            }),
            ErrorKind::ToolFailed,
        ),
        (
            TurnError::ContextBudgetExceeded {
                budget: 10,
                required: 20,
            },
            ErrorKind::ContextOverflow,
        ),
        (
            TurnError::RepeatedToolCalls {
                repeats: 3,
                tool: "read".to_string(),
            },
            ErrorKind::Repetition,
        ),
    ];

    for (error, expected) in cases {
        let message = error.to_string();

        // Act
        let run_error = RunError::from(error);

        // Assert
        assert_eq!(run_error.kind(), expected, "{message}");
        assert!(!run_error.is_retryable(), "{message}");
        assert_eq!(run_error.retry_after(), None, "{message}");
        assert_eq!(run_error.to_string(), message);
    }
}

/// Asserts each session failure's run classification and unchanged message;
/// only `Busy` failures are retryable.
fn assert_session_classifications(cases: Vec<(SessionError, ErrorKind)>) {
    for (error, expected) in cases {
        let message = error.to_string();
        let retryable = expected == ErrorKind::Busy;

        // Act
        let run_error = RunError::from(error);

        // Assert
        assert_eq!(run_error.kind(), expected, "{message}");
        assert_eq!(run_error.is_retryable(), retryable, "{message}");
        assert_eq!(run_error.to_string(), message);
    }
}

#[test]
fn classifies_session_state_and_configuration_failures() {
    // Arrange
    let cases = vec![
        (
            SessionError::StaleModel { id: "s".into() },
            ErrorKind::Conflict,
        ),
        (
            SessionError::UnsupportedModelHistory { reason: "images" },
            ErrorKind::Config,
        ),
        (
            SessionError::ImageInputExceedsHistory {
                id: "s".into(),
                input_bytes: 2,
                max_history_bytes: 1,
            },
            ErrorKind::ContextOverflow,
        ),
        (
            SessionError::Checkpoint(CheckpointError::InvalidRecord { reason: "fixture" }),
            ErrorKind::Store,
        ),
        (
            SessionError::CheckpointStale { id: "s".into() },
            ErrorKind::Conflict,
        ),
        (
            SessionError::Registry(ModelRegistryError::DuplicateKey { key: "k".into() }),
            ErrorKind::Config,
        ),
        (
            SessionError::RegistrationMismatch { id: "s".into() },
            ErrorKind::Config,
        ),
        (SessionError::ExecutionIdentityRequired, ErrorKind::Config),
        (SessionError::HostTurnConflict, ErrorKind::Conflict),
        (
            SessionError::HostTurnInProgress(host_turn_record()),
            ErrorKind::Busy,
        ),
        (
            SessionError::HostTurnStopped(host_turn_record()),
            ErrorKind::Conflict,
        ),
        (
            SessionError::AlreadyExists { id: "s".into() },
            ErrorKind::Conflict,
        ),
        (SessionError::Busy { id: "s".into() }, ErrorKind::Busy),
        (
            SessionError::ModelMismatch {
                actual_model: "b".into(),
                actual_provider: "p".into(),
                id: "s".into(),
                stored_model: "a".into(),
                stored_provider: "p".into(),
            },
            ErrorKind::Config,
        ),
        (
            SessionError::NotFound { id: "s".into() },
            ErrorKind::NotFound,
        ),
        (
            SessionError::OwnershipLost {
                id: "s".into(),
                turn_position: 0,
            },
            ErrorKind::Conflict,
        ),
        (SessionError::StorageRequired, ErrorKind::Config),
    ];

    // Act and Assert
    assert_session_classifications(cases);
}

#[test]
fn classifies_session_store_and_turn_failures() {
    // Arrange
    let cases = vec![
        (store_failure(), ErrorKind::Store),
        (SessionError::Io(io::Error::other("disk")), ErrorKind::Store),
        (
            SessionError::InvalidData {
                reason: "fixture".into(),
            },
            ErrorKind::Store,
        ),
        (
            SessionError::Migration(sqlx::migrate::MigrateError::VersionMissing(1)),
            ErrorKind::Store,
        ),
        (
            SessionError::QueryContext {
                operation: "fixture",
                source: sqlx::Error::PoolClosed,
            },
            ErrorKind::Store,
        ),
        (
            SessionError::Schema(OutputSchemaError::TooLarge),
            ErrorKind::Store,
        ),
        (
            SessionError::Turn(TurnError::Cancelled),
            ErrorKind::Cancelled,
        ),
        (
            SessionError::TurnPersistence {
                turn: TurnError::RepeatedToolCalls {
                    repeats: 3,
                    tool: "read".into(),
                },
                persistence: Box::new(store_failure()),
            },
            ErrorKind::Repetition,
        ),
    ];

    // Act and Assert
    assert_session_classifications(cases);
}

#[test]
fn harness_error_classifies_session_failures_like_runs() {
    // Arrange
    let busy = SessionError::Busy { id: "s".into() };
    let missing = SessionError::NotFound { id: "s".into() };

    // Act
    let busy = HarnessError::from(busy);
    let missing = HarnessError::from(missing);

    // Assert
    assert_eq!((busy.kind(), busy.is_retryable()), (ErrorKind::Busy, true));
    assert_eq!(
        (missing.kind(), missing.is_retryable()),
        (ErrorKind::NotFound, false)
    );
    assert_eq!(missing.to_string(), "persistent session `s` does not exist");
    assert!(missing.source().is_none());
}

#[test]
fn harness_error_chains_the_backend_source() {
    // Arrange
    let error = store_failure();

    // Act
    let error = HarnessError::from(error);

    // Assert
    assert_eq!(error.kind(), ErrorKind::Store);
    assert_eq!(
        error.source().map(ToString::to_string).as_deref(),
        Some("store offline")
    );
}

#[test]
fn classifies_provider_rejections_by_quota_and_status() {
    // Arrange
    let three_seconds = Some(Duration::from_secs(3));
    let cases = [
        (
            provider_error(Some(429), true),
            ErrorKind::QuotaExceeded,
            None,
        ),
        (
            provider_error(Some(402), true),
            ErrorKind::QuotaExceeded,
            None,
        ),
        (provider_error(Some(401), false), ErrorKind::Auth, None),
        (provider_error(Some(403), false), ErrorKind::Auth, None),
        (
            provider_error(Some(408), false),
            ErrorKind::Transport,
            three_seconds,
        ),
        (
            provider_error(Some(429), false),
            ErrorKind::RateLimited,
            three_seconds,
        ),
        (
            provider_error(Some(500), false),
            ErrorKind::Overloaded,
            three_seconds,
        ),
        (
            provider_error(Some(503), false),
            ErrorKind::Overloaded,
            three_seconds,
        ),
        (
            provider_error(Some(529), false),
            ErrorKind::Overloaded,
            three_seconds,
        ),
        (provider_error(Some(400), false), ErrorKind::Model, None),
        (provider_error(Some(404), false), ErrorKind::Model, None),
        (provider_error(None, false), ErrorKind::Model, None),
    ];

    for (error, expected_kind, expected_retry_after) in cases {
        let status = error.http_status();

        // Act
        let run_error = RunError::from(error);

        // Assert
        assert_eq!(run_error.kind(), expected_kind, "{status:?}");
        assert_eq!(
            run_error.is_retryable(),
            expected_retry_after.is_some(),
            "{status:?}"
        );
        assert_eq!(run_error.retry_after(), expected_retry_after, "{status:?}");
    }
}

#[test]
fn classifies_every_model_failure_type() {
    // Arrange
    let cases = [
        (
            ModelError::context_overflow(io::Error::other("too long")),
            ErrorKind::ContextOverflow,
            false,
        ),
        (
            classified(ModelErrorType::Transport),
            ErrorKind::Transport,
            true,
        ),
        (
            classified(ModelErrorType::InvalidProviderResponse),
            ErrorKind::Transport,
            true,
        ),
        (ModelError::InvalidResponse, ErrorKind::InvalidOutput, false),
        (
            ModelError::ResponseBodyTooLarge,
            ErrorKind::InvalidOutput,
            false,
        ),
        (
            ModelError::InvalidJson {
                reason: "eof".into(),
            },
            ErrorKind::InvalidOutput,
            false,
        ),
        (ModelError::MissingToolCall, ErrorKind::InvalidOutput, false),
        (
            ModelError::UnsupportedOutputSchema {
                reason: "fixture".into(),
            },
            ErrorKind::Config,
            false,
        ),
        (
            ModelError::UnsupportedImageInput {
                reason: "fixture".into(),
            },
            ErrorKind::Config,
            false,
        ),
        (
            ModelError::request(io::Error::other("custom model failed")),
            ErrorKind::Model,
            false,
        ),
    ];

    for (error, expected_kind, expected_retryable) in cases {
        let message = error.to_string();

        // Act
        let run_error = RunError::from(error);

        // Assert
        assert_eq!(run_error.kind(), expected_kind, "{message}");
        assert_eq!(run_error.is_retryable(), expected_retryable, "{message}");
        assert_eq!(run_error.retry_after(), None, "{message}");
    }
}

#[test]
fn turn_model_failure_keeps_the_provider_retry_delay() {
    // Arrange
    let error = TurnError::Model(provider_error(Some(429), false));

    // Act
    let run_error = RunError::from(error);

    // Assert
    assert_eq!(run_error.kind(), ErrorKind::RateLimited);
    assert!(run_error.is_retryable());
    assert_eq!(run_error.retry_after(), Some(Duration::from_secs(3)));
    assert_eq!(
        run_error.source().map(ToString::to_string).as_deref(),
        Some("provider rejected the request")
    );
}

#[test]
fn kinds_have_stable_names() {
    // Arrange
    let cases = [
        (ErrorKind::Cancelled, "cancelled"),
        (ErrorKind::RateLimited, "rate_limited"),
        (ErrorKind::Overloaded, "overloaded"),
        (ErrorKind::Auth, "auth"),
        (ErrorKind::QuotaExceeded, "quota_exceeded"),
        (ErrorKind::ContextOverflow, "context_overflow"),
        (ErrorKind::InvalidOutput, "invalid_output"),
        (ErrorKind::Transport, "transport"),
        (ErrorKind::Model, "model"),
        (ErrorKind::ToolFailed, "tool_failed"),
        (ErrorKind::Repetition, "repetition"),
        (ErrorKind::Busy, "busy"),
        (ErrorKind::Conflict, "conflict"),
        (ErrorKind::NotFound, "not_found"),
        (ErrorKind::Store, "store"),
        (ErrorKind::Config, "config"),
    ];

    for (kind, expected) in cases {
        // Act
        let name = kind.to_string();

        // Assert
        assert_eq!(name, expected);
        assert_eq!(kind.as_str(), expected);
    }
}
