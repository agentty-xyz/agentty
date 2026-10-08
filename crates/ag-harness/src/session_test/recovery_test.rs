use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::TurnOutcome;
use crate::input::TurnInput;
use crate::lifecycle::ModelResponseType;
use crate::recovery::{HostRequest, HostTurnAcquisition, HostTurnStatus};
use crate::session::tests::support::{schema, turn_options};
use crate::session::{Database, SessionError};
use crate::store::{AcquiredTurn, NewSession, SessionStore};
use crate::turn::{ModelRequestActivity, TurnReport};

#[tokio::test]
async fn recovery_acquisition_is_atomic_across_independent_sqlite_pools() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("sessions.sqlite");
    let left = Arc::new(Database::open(&path).await.expect("left"));
    let right = Arc::new(Database::open(&path).await.expect("right"));
    left.create_session(&NewSession::new("session", schema()), None, 1024)
        .await
        .expect("create");
    let request =
        HostRequest::from_configuration("id".into(), json!({"prompt":"hello"})).expect("request");
    let options = turn_options();

    // Act
    let input = TurnInput::from("hello");
    let (first, second) = tokio::join!(
        AcquiredTurn::begin_request(left.clone(), "session", &input, &options, &request, 0),
        AcquiredTurn::begin_request(right.clone(), "session", &input, &options, &request, 0),
    );

    // Assert
    let ((HostTurnAcquisition::Acquired(acquired), HostTurnAcquisition::Recorded(record))
    | (HostTurnAcquisition::Recorded(record), HostTurnAcquisition::Acquired(acquired))) =
        (first.expect("first"), second.expect("second"))
    else {
        std::panic::resume_unwind(Box::new("expected one acquisition"));
    };
    assert!(matches!(record.status, HostTurnStatus::InProgress));
    assert_eq!(acquired.owner().turn_position(), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_turn")
            .fetch_one(&left.pool)
            .await
            .expect("count"),
        1
    );
}

#[tokio::test]
async fn recovery_rejects_corrupt_terminal_data_instead_of_reexecuting() {
    // Arrange
    let database = Arc::new(Database::open_in_memory().await.expect("database"));
    database
        .create_session(&NewSession::new("session", schema()), None, 1024)
        .await
        .expect("create");
    let request = HostRequest::from_configuration("id".into(), json!({})).expect("request");
    let HostTurnAcquisition::Acquired(turn) = AcquiredTurn::begin_request(
        database.clone(),
        "session",
        &TurnInput::from("hello"),
        &turn_options(),
        &request,
        0,
    )
    .await
    .expect("turn") else {
        std::panic::resume_unwind(Box::new("expected acquisition"));
    };
    let outcome = TurnOutcome::new(
        json!({"summary":"ok"}),
        TurnReport::new(Duration::ZERO, Vec::new(), Vec::new()),
    );

    // Act / Assert
    assert!(
        SessionStore::complete_turn(database.as_ref(), turn.owner(), &[])
            .await
            .is_err()
    );
    database
        .complete_request(turn.owner(), &[], &outcome)
        .await
        .expect("complete");
    for payload in [
        None,
        Some(r#"{"version":2,"outcome":{}}"#),
        Some(r#"{"version":1,"outcome":{}}"#),
    ] {
        sqlx::query("UPDATE session_turn SET terminal_outcome = ?")
            .bind(payload)
            .execute(&database.pool)
            .await
            .expect("corrupt payload");
        assert!(matches!(
            database.load_request("session", "id").await,
            Err(SessionError::InvalidData { .. })
        ));
    }
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&database.pool)
        .await
        .expect("allow corrupt fixture");
    sqlx::query("UPDATE session_turn SET status = 'invalid'")
        .execute(&database.pool)
        .await
        .expect("corrupt state");
    assert!(matches!(
        database.load_request("session", "id").await,
        Err(SessionError::InvalidData { .. })
    ));
}

#[tokio::test]
async fn recovery_after_reopen_marks_expired_host_requests_interrupted() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("sessions.sqlite");
    let timestamp = Arc::new(AtomicI64::new(1000));
    let clock = Arc::clone(&timestamp);
    let database = Arc::new(
        Database::open_with_timestamp_source(&path, Arc::new(move || clock.load(Ordering::SeqCst)))
            .await
            .expect("database"),
    );
    database
        .create_session(&NewSession::new("session", schema()), None, 1024)
        .await
        .expect("session");
    let request = HostRequest::from_configuration("id".into(), json!({})).expect("request");
    let turn = AcquiredTurn::begin_request(
        database.clone(),
        "session",
        &TurnInput::from("hello"),
        &turn_options(),
        &request,
        0,
    )
    .await
    .expect("turn");
    timestamp.store(2000, Ordering::SeqCst);

    // Act
    let clock = Arc::clone(&timestamp);
    let reopened =
        Database::open_with_timestamp_source(&path, Arc::new(move || clock.load(Ordering::SeqCst)))
            .await
            .expect("reopen");
    let record = reopened
        .load_request("session", "id")
        .await
        .expect("lookup")
        .expect("record");
    let duplicate = AcquiredTurn::begin_request(
        Arc::new(reopened.clone()),
        "session",
        &TurnInput::from("hello"),
        &turn_options(),
        &request,
        0,
    )
    .await
    .expect("duplicate");

    // Assert
    assert!(matches!(record.status, HostTurnStatus::Interrupted { .. }));
    assert!(matches!(duplicate, HostTurnAcquisition::Recorded(_)));
    drop(turn);
}

#[tokio::test]
async fn migration_drops_removed_resume_activities_from_recorded_outcomes() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("sessions.sqlite");
    let database = Arc::new(Database::open(&path).await.expect("database"));
    database
        .create_session(&NewSession::new("session", schema()), None, 1024)
        .await
        .expect("create");
    let request = HostRequest::from_configuration("id".into(), json!({})).expect("request");
    let HostTurnAcquisition::Acquired(turn) = AcquiredTurn::begin_request(
        database.clone(),
        "session",
        &TurnInput::from("hello"),
        &turn_options(),
        &request,
        0,
    )
    .await
    .expect("turn") else {
        std::panic::resume_unwind(Box::new("expected acquisition"));
    };
    let outcome = TurnOutcome::new(
        json!({"summary":"ok"}),
        TurnReport::new(
            Duration::from_millis(9),
            vec![
                ModelRequestActivity::new(
                    None,
                    Duration::from_millis(2),
                    ModelResponseType::ToolCall,
                ),
                ModelRequestActivity::new(
                    None,
                    Duration::from_millis(3),
                    ModelResponseType::Output,
                ),
            ],
            Vec::new(),
        ),
    );
    database
        .complete_request(turn.owner(), &[], &outcome)
        .await
        .expect("complete");
    drop(turn);
    let stored: String = sqlx::query_scalar("SELECT terminal_outcome FROM session_turn")
        .fetch_one(&database.pool)
        .await
        .expect("stored outcome");
    let mut stored: Value = serde_json::from_str(&stored).expect("outcome json");
    let requests = stored["outcome"]["report"]["model_requests"]
        .as_array_mut()
        .expect("model requests");
    let mut rejected_resume = requests[0].clone();
    rejected_resume["response_type"] = json!("ResumeUnavailable");
    requests.insert(0, rejected_resume);
    // Restore the pre-migration-012 schema holding a native-resume fallback.
    for statement in [
        "ALTER TABLE session ADD COLUMN provider_session_id TEXT",
        "DELETE FROM _sqlx_migrations WHERE version = 12",
    ] {
        sqlx::query(statement)
            .execute(&database.pool)
            .await
            .expect("pre-migration schema");
    }
    sqlx::query("UPDATE session_turn SET terminal_outcome = ?")
        .bind(stored.to_string())
        .execute(&database.pool)
        .await
        .expect("legacy outcome");
    database.pool.close().await;

    // Act
    let migrated = Database::open(&path).await.expect("migrated database");
    let record = migrated
        .load_request("session", "id")
        .await
        .expect("lookup")
        .expect("record");

    // Assert
    let HostTurnStatus::Completed(recorded) = record.status else {
        std::panic::resume_unwind(Box::new("expected completed request"));
    };
    assert_eq!(recorded, outcome);
}
