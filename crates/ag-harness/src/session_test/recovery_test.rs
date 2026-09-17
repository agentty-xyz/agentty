use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde_json::json;

use crate::session::tests::support::{schema, turn_options};
use crate::session::{Database, SessionError};
use crate::{
    HostRequest, HostTurnAcquisition, HostTurnStatus, NewSession, SessionStore, TurnOutcome,
    TurnReport,
};

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
    let (first, second) = tokio::join!(
        left.begin_request(left.clone(), "session", "hello", &options, &request),
        right.begin_request(right.clone(), "session", "hello", &options, &request),
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
    let HostTurnAcquisition::Acquired(turn) = database
        .begin_request(
            database.clone(),
            "session",
            "hello",
            &turn_options(),
            &request,
        )
        .await
        .expect("turn")
    else {
        std::panic::resume_unwind(Box::new("expected acquisition"));
    };
    let outcome = TurnOutcome::new(
        json!({"summary":"ok"}),
        TurnReport::new(Duration::ZERO, Vec::new(), Vec::new()),
    );

    // Act / Assert
    assert!(
        SessionStore::complete_turn(database.as_ref(), turn.owner(), &[], None)
            .await
            .is_err()
    );
    database
        .complete_request(turn.owner(), &[], None, &outcome)
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
    let turn = database
        .begin_request(
            database.clone(),
            "session",
            "hello",
            &turn_options(),
            &request,
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
    let duplicate = reopened
        .begin_request(
            Arc::new(reopened.clone()),
            "session",
            "hello",
            &turn_options(),
            &request,
        )
        .await
        .expect("duplicate");

    // Assert
    assert!(matches!(record.status, HostTurnStatus::Interrupted { .. }));
    assert!(matches!(duplicate, HostTurnAcquisition::Recorded(_)));
    drop(turn);
}
