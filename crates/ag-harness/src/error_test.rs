//! Failure classification unit tests proving every turn, session, model, and
//! build failure maps to one kind with the right retry signal.

#[cfg(feature = "sqlite")]
use std::borrow::Cow;
use std::error::Error;
use std::io;
use std::time::Duration;

use serde_json::json;

use crate::bash::{BashError, CommandCleanupScope, CommandOutcome, CommandTermination};
use crate::error::{ErrorKind, ErrorKindName, ErrorKindWireError, HarnessError, RunError};
use crate::limit::LimitKind;
use crate::model::{ModelError, ModelErrorType, ModelRegistryError, ProviderStatus};
use crate::recovery::{HostRequest, HostTurnRecord, HostTurnStatus};
use crate::sandbox::SandboxUnavailableReason;
use crate::store::CheckpointError;
use crate::tool::{ReadError, WriteError};
use crate::{OutputSchemaError, RepositoryError, SessionError, TurnError, TurnInputError};

/// Database failure carrying a fixed backend result code.
#[cfg(feature = "sqlite")]
#[derive(Debug, thiserror::Error)]
#[error("database failure {code:?}")]
struct CodedDatabaseError {
    code: Option<&'static str>,
}

#[cfg(feature = "sqlite")]
impl sqlx::error::DatabaseError for CodedDatabaseError {
    fn message(&self) -> &'static str {
        "database failure"
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        self.code.map(Cow::Borrowed)
    }

    fn as_error(&self) -> &(dyn Error + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn Error + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn Error + Send + Sync + 'static> {
        self
    }

    fn kind(&self) -> sqlx::error::ErrorKind {
        sqlx::error::ErrorKind::Other
    }
}

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

fn provider_error(
    http_status: Option<u16>,
    quota_exceeded: bool,
    retry_after: Option<Duration>,
) -> ModelError {
    ModelError::classified_request(
        ModelErrorType::Provider,
        ProviderStatus {
            http_status,
            quota_exceeded,
            retry_after,
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

#[cfg(feature = "sqlite")]
fn database_failure(code: Option<&'static str>) -> SessionError {
    SessionError::QueryContext {
        operation: "fixture",
        source: sqlx::Error::Database(Box::new(CodedDatabaseError { code })),
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
        (TurnError::Bash(BashError::InvalidPolicy), ErrorKind::Config),
        (TurnError::Bash(BashError::Unavailable), ErrorKind::Config),
        (
            TurnError::Bash(BashError::InvalidArguments),
            ErrorKind::ToolFailed,
        ),
        (TurnError::Bash(BashError::Execution), ErrorKind::ToolFailed),
        (TurnError::Bash(BashError::Cleanup), ErrorKind::ToolFailed),
        (
            TurnError::MessageJournal {
                source: Box::new(store_failure()),
            },
            ErrorKind::Store,
        ),
        (
            TurnError::UsageJournal {
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
        (
            TurnError::ApprovalTimeout {
                timeout: Duration::from_secs(1),
                tool: "deploy".to_string(),
            },
            ErrorKind::ApprovalTimeout,
        ),
        (
            TurnError::SandboxUnavailable(SandboxUnavailableReason::BubblewrapMissing),
            ErrorKind::SandboxUnavailable,
        ),
        (
            TurnError::limit_reached(LimitKind::ToolCalls),
            ErrorKind::BudgetExhausted(LimitKind::ToolCalls),
        ),
        (
            TurnError::ToolFailed {
                source: "connection refused".into(),
                tool: "fetch".to_string(),
            },
            ErrorKind::ToolFailed,
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
        assert_eq!(run_error.retry_after(), None, "{message}");
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
            SessionError::RegistrationMismatch {
                id: "s".into(),
                stored: None,
            },
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
            SessionError::TurnNotFound {
                id: "s".into(),
                turn: 3,
            },
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
            SessionError::Schema(OutputSchemaError::TooLarge),
            ErrorKind::Store,
        ),
        (
            SessionError::Turn(TurnError::Cancelled),
            ErrorKind::Cancelled,
        ),
        (
            SessionError::TurnPersistence {
                turn: TurnError::Model(provider_error(
                    Some(429),
                    false,
                    Some(Duration::from_secs(3)),
                )),
                persistence: Box::new(store_failure()),
            },
            ErrorKind::Store,
        ),
    ];

    // Act and Assert
    assert_session_classifications(cases);
}

#[cfg(feature = "sqlite")]
#[test]
fn classifies_sqlite_failures_as_store_failures() {
    // Arrange
    let cases = vec![
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
    ];

    // Act and Assert
    assert_session_classifications(cases);
}

#[cfg(feature = "sqlite")]
#[test]
fn classifies_store_contention_as_busy() {
    // Arrange
    let cases = vec![
        (
            SessionError::QueryContext {
                operation: "fixture",
                source: sqlx::Error::PoolTimedOut,
            },
            ErrorKind::Busy,
        ),
        (database_failure(Some("5")), ErrorKind::Busy),
        (database_failure(Some("517")), ErrorKind::Busy),
        (database_failure(Some("6")), ErrorKind::Busy),
        (database_failure(Some("19")), ErrorKind::Store),
        (database_failure(Some("busy")), ErrorKind::Store),
        (database_failure(None), ErrorKind::Store),
    ];

    // Act and Assert
    assert_session_classifications(cases);
}

#[test]
fn harness_error_classifies_session_failures_like_runs() {
    // Arrange
    let busy = SessionError::Busy { id: "s".into() };
    let missing = SessionError::NotFound { id: "s".into() };
    let rate_limited = SessionError::Turn(TurnError::Model(provider_error(
        Some(429),
        false,
        Some(Duration::from_secs(3)),
    )));

    // Act
    let busy = HarnessError::from(busy);
    let missing = HarnessError::from(missing);
    let rate_limited = HarnessError::from(rate_limited);

    // Assert
    assert_eq!((busy.kind(), busy.is_retryable()), (ErrorKind::Busy, true));
    assert_eq!(
        (missing.kind(), missing.is_retryable()),
        (ErrorKind::NotFound, false)
    );
    assert_eq!(missing.to_string(), "persistent session `s` does not exist");
    assert!(missing.source().is_none());
    assert_eq!(rate_limited.kind(), ErrorKind::RateLimited);
    assert_eq!(rate_limited.retry_after(), Some(Duration::from_secs(3)));
}

#[test]
fn harness_error_classifies_build_failures_as_configuration() {
    // Arrange
    let cases = [
        HarnessError::from(OutputSchemaError::TooLarge),
        HarnessError::from(TurnInputError::EmptyImage),
        HarnessError::from(RepositoryError::RootIsGitAdministrative {
            path: "/repo/.git".into(),
        }),
        HarnessError::from(ModelRegistryError::UnknownKey { key: "k".into() }),
    ];

    for error in cases {
        // Act
        let classification = (error.kind(), error.is_retryable(), error.retry_after());

        // Assert
        assert_eq!(classification, (ErrorKind::Config, false, None), "{error}");
    }
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
fn converted_errors_stay_reachable() {
    // Arrange
    let stopped = HarnessError::from(SessionError::HostTurnStopped(host_turn_record()));
    let budget = RunError::from(TurnError::ContextBudgetExceeded {
        budget: 10,
        required: 20,
    });

    // Act
    let stopped_ref = matches!(
        stopped.get_ref().downcast_ref::<SessionError>(),
        Some(SessionError::HostTurnStopped(record)) if record.turn_position == 0
    );
    let budget_ref = budget.get_ref().downcast_ref::<TurnError>().is_some();
    let stopped = stopped.into_inner().downcast::<SessionError>();
    let budget = budget.into_inner().downcast::<TurnError>();

    // Assert
    assert!(stopped_ref);
    assert!(budget_ref);
    assert!(matches!(
        stopped.as_deref(),
        Ok(SessionError::HostTurnStopped(_))
    ));
    assert!(matches!(
        budget.as_deref(),
        Ok(TurnError::ContextBudgetExceeded {
            budget: 10,
            required: 20
        })
    ));
}

#[test]
fn classifies_provider_rejections_by_quota_and_status() {
    // Arrange
    let three_seconds = Some(Duration::from_secs(3));
    let cases = [
        (
            Some(429),
            true,
            three_seconds,
            ErrorKind::QuotaExceeded,
            false,
        ),
        (Some(402), true, None, ErrorKind::QuotaExceeded, false),
        (Some(401), false, None, ErrorKind::Auth, false),
        (Some(403), false, None, ErrorKind::Auth, false),
        (Some(408), false, three_seconds, ErrorKind::Transport, true),
        (
            Some(429),
            false,
            three_seconds,
            ErrorKind::RateLimited,
            true,
        ),
        (Some(429), false, None, ErrorKind::RateLimited, true),
        (Some(500), false, three_seconds, ErrorKind::Overloaded, true),
        (Some(502), false, None, ErrorKind::Overloaded, true),
        (Some(503), false, None, ErrorKind::Overloaded, true),
        (Some(504), false, None, ErrorKind::Overloaded, true),
        (Some(529), false, three_seconds, ErrorKind::Overloaded, true),
        (Some(501), false, three_seconds, ErrorKind::Other, false),
        (Some(505), false, None, ErrorKind::Other, false),
        (Some(400), false, None, ErrorKind::Other, false),
        (Some(404), false, None, ErrorKind::Other, false),
        (None, false, None, ErrorKind::Other, false),
    ];

    for (status, quota_exceeded, retry_after, expected_kind, expected_retryable) in cases {
        let error = provider_error(status, quota_exceeded, retry_after);
        let expected_retry_after = retry_after.filter(|_| expected_retryable);

        // Act
        let run_error = RunError::from(error);

        // Assert
        assert_eq!(run_error.kind(), expected_kind, "{status:?}");
        assert_eq!(run_error.is_retryable(), expected_retryable, "{status:?}");
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
            ErrorKind::InvalidOutput,
            false,
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
            ErrorKind::Other,
            false,
        ),
        (
            ModelError::quota_exceeded(io::Error::other("no credit")),
            ErrorKind::QuotaExceeded,
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
    let error = TurnError::Model(provider_error(
        Some(429),
        false,
        Some(Duration::from_secs(3)),
    ));

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
        (
            ErrorKind::BudgetExhausted(LimitKind::Tokens),
            "budget_exhausted",
        ),
        (ErrorKind::RateLimited, "rate_limited"),
        (ErrorKind::Overloaded, "overloaded"),
        (ErrorKind::Auth, "auth"),
        (ErrorKind::QuotaExceeded, "quota_exceeded"),
        (ErrorKind::ContextOverflow, "context_overflow"),
        (ErrorKind::InvalidOutput, "invalid_output"),
        (ErrorKind::Transport, "transport"),
        (ErrorKind::Other, "other"),
        (ErrorKind::ToolFailed, "tool_failed"),
        (ErrorKind::Repetition, "repetition"),
        (ErrorKind::ApprovalTimeout, "approval_timeout"),
        (ErrorKind::Busy, "busy"),
        (ErrorKind::Conflict, "conflict"),
        (ErrorKind::NotFound, "not_found"),
        (ErrorKind::Store, "store"),
        (ErrorKind::Config, "config"),
        (ErrorKind::SandboxUnavailable, "sandbox_unavailable"),
    ];

    for (kind, expected) in cases {
        // Act
        let name = kind.to_string();
        let serialized = serde_json::to_value(kind).expect("kind serializes");
        let restored = serde_json::from_value::<ErrorKind>(serialized.clone());

        // Assert
        assert_eq!(name, expected);
        assert_eq!(kind.as_str(), expected);
        assert_eq!(serialized, json!(expected));
        if matches!(kind, ErrorKind::BudgetExhausted(_)) {
            assert!(restored.is_err(), "a bare name cannot restore its limit");
        } else {
            assert_eq!(restored.ok(), Some(kind));
        }
    }
}

#[test]
fn wire_kind_names_take_a_limit_only_for_budget_exhausted() {
    // Arrange
    let limit = Some(LimitKind::WallTime);

    // Act
    let results = [
        ErrorKindName::BudgetExhausted.with_limit(limit),
        ErrorKindName::BudgetExhausted.with_limit(None),
        ErrorKindName::Cancelled.with_limit(limit),
    ];

    // Assert
    assert_eq!(
        results,
        [
            Ok(ErrorKind::BudgetExhausted(LimitKind::WallTime)),
            Err(ErrorKindWireError::MissingLimit),
            Err(ErrorKindWireError::UnexpectedLimit),
        ]
    );
    assert!(!ErrorKind::BudgetExhausted(LimitKind::WallTime).is_retryable());
}
