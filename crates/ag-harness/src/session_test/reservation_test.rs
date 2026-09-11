use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use sqlx::sqlite::SqlitePoolOptions;
use tempfile::tempdir;

use super::support::{
    ReservationCommitControl, active_turn_owner, complete_native_turn, schema, turn,
};
use crate::model::{ModelError, ModelMessage};
use crate::session::{
    Database, EncodedMessage, NewSession, SessionError, TURN_LEASE_SECONDS, TimestampSource,
    connect_options, interrupt_owned_turn,
};
use crate::turn::TurnError;

#[tokio::test]
async fn owner_token_migration_preserves_existing_turns() {
    // Arrange
    let temp_dir = tempdir().expect("temporary directory should be created");
    let database_path = temp_dir.path().join("harness.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options(&database_path))
        .await
        .expect("legacy database should open");
    sqlx::migrate!("./migrations")
        .run_to(2, &pool)
        .await
        .expect("legacy migrations should run");
    sqlx::query(
        r"
INSERT INTO session (id, output_schema, max_history_bytes, created_at, updated_at)
VALUES ('session-a', '{}', 100000, 10, 10)
",
    )
    .execute(&pool)
    .await
    .expect("legacy session should be inserted");
    sqlx::query(
        r"
INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at
)
VALUES ('session-a', 0, 'running', NULL, 310, 10, 10)
",
    )
    .execute(&pool)
    .await
    .expect("legacy turn should be inserted");
    pool.close().await;

    // Act
    let database = Database::open(&database_path)
        .await
        .expect("upgraded database should open");
    let turn = sqlx::query_as::<_, (String, Option<Vec<u8>>)>(
        "SELECT status, owner_token FROM session_turn WHERE session_id = 'session-a'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("upgraded turn should load");

    // Assert
    assert_eq!(turn, ("running".to_string(), None));
}

#[tokio::test]
async fn beginning_a_turn_for_a_missing_session_reports_not_found() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");

    // Act
    let error = database
        .begin_turn("missing", "prompt")
        .await
        .err()
        .expect("missing session should fail");

    // Assert
    assert!(matches!(error, SessionError::NotFound { .. }));
}

#[tokio::test]
async fn beginning_a_turn_preserves_non_unique_database_failures() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    sqlx::query(
        r"
CREATE TRIGGER reject_session_turn
BEFORE INSERT ON session_turn
BEGIN
    SELECT RAISE(ABORT, 'turn rejected');
END
",
    )
    .execute(&database.pool)
    .await
    .expect("trigger should be created");

    // Act
    let error = database
        .begin_turn("session-a", "prompt")
        .await
        .err()
        .expect("database failure should be preserved");

    // Assert
    assert!(matches!(error, SessionError::QueryContext { .. }));
}

#[tokio::test]
async fn beginning_a_turn_does_not_reserve_when_history_loading_fails() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    database
        .append_turn("session-a", &turn("question", "answer"))
        .await
        .expect("turn should be appended");
    sqlx::query("UPDATE session_message SET kind = 'unknown' WHERE session_id = 'session-a'")
        .execute(&database.pool)
        .await
        .expect("history should be corrupted");

    // Act
    let error = database
        .begin_turn("session-a", "new prompt")
        .await
        .err()
        .expect("invalid history should fail acquisition");
    let active_turns = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM session_turn WHERE status IN ('pending', 'running')",
    )
    .fetch_one(&database.pool)
    .await
    .expect("active turn count should load");
    let message_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM session_message WHERE session_id = 'session-a'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("message count should load");

    // Assert
    assert!(matches!(error, SessionError::InvalidData { .. }));
    assert_eq!(active_turns, 0);
    assert_eq!(message_count, 2);
}

#[tokio::test]
async fn beginning_a_turn_calculates_the_lease_when_reserving() {
    // Arrange
    let now = Arc::new(AtomicI64::new(10));
    let timestamp_source: Arc<dyn TimestampSource> = {
        let now = Arc::clone(&now);

        Arc::new(move || now.fetch_add(10, Ordering::SeqCst))
    };
    let database = Database::open_in_memory_with_timestamp_source(timestamp_source)
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");

    // Act
    let acquired = database
        .begin_turn("session-a", "prompt")
        .await
        .expect("turn should begin");
    let row = sqlx::query_as::<_, (String, i64, i64, i64)>(
        r"
SELECT status, lease_expires_at, created_at, updated_at
FROM session_turn
WHERE session_id = ? AND turn_position = ?
",
    )
    .bind("session-a")
    .bind(acquired.turn_position)
    .fetch_one(&database.pool)
    .await
    .expect("turn lifecycle should load");

    // Assert
    assert_eq!(row, ("running".to_string(), 330, 30, 30));
}

#[tokio::test]
async fn reserving_a_turn_rejects_a_stale_acquisition_snapshot() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    let acquisition = database
        .load_turn_acquisition("session-a")
        .await
        .expect("turn acquisition should load");
    database
        .append_turn("session-a", &turn("question", "answer"))
        .await
        .expect("turn should be appended");
    let message = EncodedMessage::from_message(&ModelMessage::User("prompt".to_string()))
        .expect("message should encode");

    // Act
    let reserved = database
        .reserve_turn("session-a", &message, &acquisition)
        .await
        .expect("reservation should be checked");
    let active_turns = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM session_turn WHERE status IN ('pending', 'running')",
    )
    .fetch_one(&database.pool)
    .await
    .expect("active turn count should load");

    // Assert
    assert!(reserved.is_none());
    assert_eq!(active_turns, 0);
}

#[tokio::test]
async fn reserving_a_turn_for_a_removed_session_reports_not_found() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    let acquisition = database
        .load_turn_acquisition("session-a")
        .await
        .expect("turn acquisition should load");
    sqlx::query("DELETE FROM session WHERE id = 'session-a'")
        .execute(&database.pool)
        .await
        .expect("session should be removed");
    let message = EncodedMessage::from_message(&ModelMessage::User("prompt".to_string()))
        .expect("message should encode");

    // Act
    let result = database
        .reserve_turn("session-a", &message, &acquisition)
        .await;

    // Assert
    assert!(matches!(result, Err(SessionError::NotFound { .. })));
}

#[tokio::test]
async fn cancelling_turn_acquisition_leaves_no_active_turn() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database = Database::open(&directory.path().join("harness.db"))
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    let mut blocker = database
        .pool
        .begin()
        .await
        .expect("blocking transaction should begin");
    sqlx::query("UPDATE session SET updated_at = updated_at WHERE id = 'session-a'")
        .execute(&mut *blocker)
        .await
        .expect("blocking transaction should hold the writer lock");

    // Act
    let cancellation = tokio::time::timeout(
        Duration::from_millis(50),
        database.begin_turn("session-a", "cancelled"),
    )
    .await;
    blocker
        .rollback()
        .await
        .expect("blocking transaction should roll back");
    let acquired = database
        .begin_turn("session-a", "replacement")
        .await
        .expect("replacement turn should begin immediately");

    // Assert
    assert!(cancellation.is_err());
    assert_eq!(acquired.turn_position, 0);
}

#[tokio::test]
async fn cancelling_after_commit_recovers_the_owned_turn_immediately() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let mut database = Database::open(&database_path)
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    let mut replacement_database = Database::open(&database_path)
        .await
        .expect("replacement database should open");
    let commit_control = Arc::new(ReservationCommitControl::paused());
    database.reservation_observer = commit_control.clone();
    replacement_database.reservation_observer = commit_control.clone();

    // Act
    let cancellation = tokio::time::timeout(
        Duration::from_secs(1),
        database.begin_turn("session-a", "cancelled"),
    )
    .await;
    let commit_seen = commit_control.commit_seen.load(Ordering::SeqCst);
    let acquired = replacement_database
        .begin_turn("session-a", "replacement")
        .await
        .expect("replacement turn should begin immediately");
    let turns = sqlx::query_as::<_, (i64, String)>(
        r"
SELECT turn_position, status
FROM session_turn
WHERE session_id = 'session-a'
ORDER BY turn_position
",
    )
    .fetch_all(&replacement_database.pool)
    .await
    .expect("turn states should load");

    // Assert
    assert!(cancellation.is_err());
    assert!(commit_seen);
    assert_eq!(acquired.turn_position, 1);
    assert_eq!(
        turns,
        vec![(0, "interrupted".to_string()), (1, "running".to_string())]
    );
}

#[tokio::test]
async fn registered_cancelled_owner_preserves_its_reason_during_recovery() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    complete_native_turn(&database, "native-session").await;
    let abandoned = database
        .begin_turn("session-a", "abandoned")
        .await
        .expect("turn should begin");
    let mut owner = active_turn_owner(&database, "session-a", abandoned.turn_position).await;
    owner.interruption_error_type = "cancelled";
    database.abandoned_turns.register(owner);

    // Act
    let replacement = database
        .begin_turn("session-a", "replacement")
        .await
        .expect("replacement turn should begin");
    let turns = sqlx::query_as::<_, (i64, String, Option<String>)>(
        r"
SELECT turn_position, status, error_type
FROM session_turn
WHERE session_id = 'session-a'
ORDER BY turn_position
",
    )
    .fetch_all(&database.pool)
    .await
    .expect("turn states should load");

    // Assert
    assert_eq!(replacement.turn_position, 2);
    assert!(replacement.provider_session_id.is_none());
    assert_eq!(
        turns,
        vec![
            (0, "completed".to_string(), None),
            (1, "interrupted".to_string(), Some("cancelled".to_string())),
            (2, "running".to_string(), None),
        ]
    );
}

#[tokio::test]
async fn stopped_ownership_monitor_reports_ownership_loss() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    let mut acquired = database
        .begin_turn("session-a", "prompt")
        .await
        .expect("turn should begin");
    acquired
        .guard
        .renewal_task
        .take()
        .expect("ownership monitor should be running")
        .abort();

    // Act
    let error = acquired.guard.ownership_failure().await;
    let repeated_error = acquired.guard.ownership_failure().await;

    // Assert
    assert!(matches!(
        error,
        SessionError::OwnershipLost {
            ref id,
            turn_position: 0,
        } if id == "session-a"
    ));
    assert!(matches!(
        repeated_error,
        SessionError::OwnershipLost {
            ref id,
            turn_position: 0,
        } if id == "session-a"
    ));
}

#[tokio::test]
async fn abandoned_owners_are_scoped_to_their_database() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let first_database = Database::open(&directory.path().join("first.db"))
        .await
        .expect("first database should open");
    let second_database = Database::open(&directory.path().join("second.db"))
        .await
        .expect("second database should open");
    for database in [&first_database, &second_database] {
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
    }
    let first_turn = first_database
        .begin_turn("session-a", "first abandoned")
        .await
        .expect("first turn should begin");
    let second_turn = second_database
        .begin_turn("session-a", "second abandoned")
        .await
        .expect("second turn should begin");
    let first_owner =
        active_turn_owner(&first_database, "session-a", first_turn.turn_position).await;
    let second_owner =
        active_turn_owner(&second_database, "session-a", second_turn.turn_position).await;
    first_database.abandoned_turns.register(first_owner);
    second_database.abandoned_turns.register(second_owner);

    // Act
    let second_replacement = second_database
        .begin_turn("session-a", "second replacement")
        .await
        .expect("second replacement should begin");
    let first_owners = first_database
        .abandoned_turns
        .for_session(&first_database.identity, "session-a");
    let first_replacement = first_database
        .begin_turn("session-a", "first replacement")
        .await
        .expect("first replacement should begin");

    // Assert
    assert_eq!(second_replacement.turn_position, 1);
    assert_eq!(first_owners.len(), 1);
    assert_eq!(first_replacement.turn_position, 1);
}

#[tokio::test]
async fn failing_or_completing_a_turn_that_is_not_running_reports_invalid_data() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    let turn_position = database
        .begin_turn("session-a", "prompt")
        .await
        .expect("turn should begin")
        .turn_position;
    database
        .fail_turn(
            "session-a",
            turn_position,
            &TurnError::Model(ModelError::InvalidResponse),
        )
        .await
        .expect("turn should fail");

    // Act
    let error = database
        .complete_turn("session-a", turn_position, &[], None, &[], None)
        .await
        .expect_err("non-running turn should fail");
    let repeated_failure = database
        .fail_turn(
            "session-a",
            turn_position,
            &TurnError::Model(ModelError::InvalidResponse),
        )
        .await
        .expect_err("repeated turn failure should fail");

    // Assert
    assert!(matches!(error, SessionError::InvalidData { .. }));
    assert!(matches!(repeated_failure, SessionError::InvalidData { .. }));
}

#[tokio::test]
async fn database_recovers_expired_active_turns_as_interrupted() {
    // Arrange
    let now = Arc::new(AtomicI64::new(10));
    let timestamp_source: Arc<dyn TimestampSource> = {
        let now = Arc::clone(&now);

        Arc::new(move || now.load(Ordering::SeqCst))
    };
    let database = Database::open_in_memory_with_timestamp_source(timestamp_source)
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    complete_native_turn(&database, "native-session").await;
    let mut abandoned = database
        .begin_turn("session-a", "abandoned")
        .await
        .expect("turn should begin");
    abandoned.guard.disarm();
    let active = database
        .load_session("session-a")
        .await
        .expect("active session should load");
    assert_eq!(
        active.provider_session_id.as_deref(),
        Some("native-session")
    );
    now.store(10 + TURN_LEASE_SECONDS + 1, Ordering::SeqCst);

    // Act
    let loaded = database
        .load_session("session-a")
        .await
        .expect("session should load");
    let replacement = database
        .begin_turn("session-a", "replacement")
        .await
        .expect("replacement turn should begin");
    let status = sqlx::query_scalar::<_, String>(
        "SELECT status FROM session_turn WHERE session_id = ? AND turn_position = ?",
    )
    .bind("session-a")
    .bind(abandoned.turn_position)
    .fetch_one(&database.pool)
    .await
    .expect("turn status should load");

    // Assert
    assert_eq!(loaded.turns, vec![turn("first", "first")]);
    assert!(loaded.provider_session_id.is_none());
    assert!(replacement.provider_session_id.is_none());
    assert_eq!(status, "interrupted");
    assert_eq!(replacement.turn_position, abandoned.turn_position + 1);
}

#[tokio::test]
async fn interruption_rolls_back_when_clearing_continuation_fails() {
    for expired in [false, true] {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(&NewSession::new("session-a", schema()), None, 100_000)
            .await
            .expect("session should be created");
        complete_native_turn(&database, "native-session").await;
        let mut acquired = database
            .begin_turn("session-a", "abandoned")
            .await
            .expect("turn should begin");
        acquired.guard.disarm();
        let owner = active_turn_owner(&database, "session-a", acquired.turn_position).await;
        sqlx::query("UPDATE session_turn SET lease_expires_at = 0 WHERE status = 'running'")
            .execute(&database.pool)
            .await
            .expect("lease should expire");
        sqlx::query(
            r"
CREATE TRIGGER reject_continuation_clear
BEFORE UPDATE OF provider_session_id ON session
BEGIN
    SELECT RAISE(ABORT, 'injected continuation failure');
END
",
        )
        .execute(&database.pool)
        .await
        .expect("failure trigger should be created");

        // Act
        let failed = if expired {
            database.recover_stale_turns("session-a").await.is_err()
        } else {
            let mut transaction = database
                .pool
                .begin()
                .await
                .expect("transaction should begin");
            let result = interrupt_owned_turn(&mut transaction, &owner, 20).await;
            transaction
                .rollback()
                .await
                .expect("transaction should roll back");

            result.is_err()
        };
        let state = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, provider_session_id FROM session_turn JOIN session ON session.id = \
             session_id WHERE turn_position = 1",
        )
        .fetch_one(&database.pool)
        .await
        .expect("state should load");

        // Assert
        assert!(failed);
        assert_eq!(
            state,
            ("running".to_string(), Some("native-session".to_string()))
        );
    }
}

#[tokio::test]
async fn delayed_or_unowned_cleanup_preserves_provider_continuation() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    complete_native_turn(&database, "native-session").await;
    let mut acquired = database
        .begin_turn("session-a", "pending")
        .await
        .expect("turn should begin");
    acquired.guard.disarm();
    let owner = active_turn_owner(&database, "session-a", acquired.turn_position).await;
    let mut wrong_owner = owner.clone();
    wrong_owner.token = vec![0];
    let mut transaction = database
        .pool
        .begin()
        .await
        .expect("transaction should begin");

    // Act
    interrupt_owned_turn(&mut transaction, &wrong_owner, 20)
        .await
        .expect("unowned cleanup should succeed");
    transaction
        .commit()
        .await
        .expect("transaction should commit");
    let active = database
        .load_session("session-a")
        .await
        .expect("session should load");
    database
        .complete_turn(
            "session-a",
            acquired.turn_position,
            &[],
            Some("replacement-session"),
            &[],
            None,
        )
        .await
        .expect("turn should complete");
    let mut transaction = database
        .pool
        .begin()
        .await
        .expect("transaction should begin");
    interrupt_owned_turn(&mut transaction, &owner, 30)
        .await
        .expect("delayed cleanup should succeed");
    transaction
        .commit()
        .await
        .expect("transaction should commit");
    let completed = database
        .load_session("session-a")
        .await
        .expect("session should load");

    // Assert
    assert_eq!(
        active.provider_session_id.as_deref(),
        Some("native-session")
    );
    assert_eq!(
        completed.provider_session_id.as_deref(),
        Some("replacement-session")
    );
}
