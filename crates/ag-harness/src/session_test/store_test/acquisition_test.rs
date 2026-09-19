use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use tokio::sync::Notify;

use super::support::{AcquisitionGate, CommitGate};
use crate::session::tests::support::{schema, turn_options};
use crate::session::{Database, NewSession, SessionError};
use crate::store::SessionStore;

#[tokio::test]
async fn abandoned_acquisition_acknowledgement_is_recovered_across_reopen() {
    // Arrange
    let directory = tempdir().expect("directory");
    let path = directory.path().join("session.db");
    let mut database = Database::open(&path).await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 100_000)
        .await
        .expect("session");
    let gate = Arc::new(AcquisitionGate {
        entered: Notify::new(),
        release: Notify::new(),
    });
    database.reservation_observer = gate.clone();
    let task = tokio::spawn(async move {
        database
            .begin_turn(
                Arc::new(database.clone()),
                "session",
                "lost",
                &turn_options(),
                0,
            )
            .await
    });
    gate.entered.notified().await;

    // Act
    task.abort();
    assert!(
        task.await
            .err()
            .expect("cancelled acquisition")
            .is_cancelled()
    );
    let database = Database::open(&path).await.expect("reopen");
    let mut replacement = database
        .begin_turn(
            Arc::new(database.clone()),
            "session",
            "replacement",
            &turn_options(),
            0,
        )
        .await
        .expect("replacement");
    replacement
        .guard
        .complete(&[], None)
        .await
        .expect("complete replacement");
    let states = sqlx::query_as::<_, (i64, String)>(
        "SELECT turn_position, status FROM session_turn ORDER BY turn_position",
    )
    .fetch_all(&database.pool)
    .await
    .expect("turns");

    // Assert
    assert_eq!(
        states,
        vec![(0, "interrupted".into()), (1, "completed".into())]
    );
}

#[tokio::test]
async fn failed_acquisition_commit_never_leaves_a_reservation() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 100_000)
        .await
        .expect("session");
    sqlx::raw_sql(
        "CREATE TABLE missing_parent(id INTEGER PRIMARY KEY);\nCREATE TABLE deferred_child(parent \
         INTEGER REFERENCES missing_parent(id) DEFERRABLE INITIALLY DEFERRED);\nCREATE TRIGGER \
         reject_commit AFTER INSERT ON session_message BEGIN INSERT INTO deferred_child VALUES \
         (1); END;",
    )
    .execute(&database.pool)
    .await
    .expect("deferred commit failure");

    // Act
    let result = database
        .begin_turn(
            Arc::new(database.clone()),
            "session",
            "rejected",
            &turn_options(),
            0,
        )
        .await;
    sqlx::query("DROP TRIGGER reject_commit")
        .execute(&database.pool)
        .await
        .expect("remove fault");
    let replacement = database
        .begin_turn(
            Arc::new(database.clone()),
            "session",
            "replacement",
            &turn_options(),
            0,
        )
        .await
        .expect("replacement");
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_turn")
        .fetch_one(&database.pool)
        .await
        .expect("count");

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::QueryContext {
            operation: "reserve persistent session turn",
            ..
        })
    ));
    assert_eq!(replacement.guard.owner.turn_position, 0);
    assert_eq!(count, 1);
}

#[tokio::test]
async fn cancelled_waiter_retains_reservation_until_commit_and_cleanup_settle() {
    // Arrange
    let mut database = Database::open_in_memory().await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 100_000)
        .await
        .expect("session");
    let gate = Arc::new(CommitGate {
        entered: Notify::new(),
        fail: false,
        release: Notify::new(),
    });
    let mut gated = database.clone();
    gated.reservation_observer = gate.clone();
    let task = tokio::spawn(async move {
        gated
            .begin_turn(
                Arc::new(gated.clone()),
                "session",
                "abandoned",
                &turn_options(),
                0,
            )
            .await
    });
    gate.entered.notified().await;

    // Act
    task.abort();
    assert!(task.await.err().expect("cancelled waiter").is_cancelled());
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status = sqlx::query_scalar::<_, String>(
                "SELECT status FROM session_turn WHERE turn_position = 0",
            )
            .fetch_optional(&database.pool)
            .await
            .expect("state");
            if status.as_deref() == Some("interrupted") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retained committer cleans up");
    database.reservation_observer = Arc::new(());
    let replacement = database
        .begin_turn(
            Arc::new(database.clone()),
            "session",
            "replacement",
            &turn_options(),
            0,
        )
        .await
        .expect("replacement");

    // Assert
    assert_eq!(replacement.guard.owner.turn_position, 1);
}

#[tokio::test]
async fn reservation_task_failure_is_reported_without_leaving_an_active_turn() {
    // Arrange
    let mut database = Database::open_in_memory().await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 100_000)
        .await
        .expect("session");
    let gate = Arc::new(CommitGate {
        entered: Notify::new(),
        fail: true,
        release: Notify::new(),
    });
    database.reservation_observer = gate.clone();
    let gated = database.clone();
    let task = tokio::spawn(async move {
        gated
            .begin_turn(
                Arc::new(gated.clone()),
                "session",
                "failed",
                &turn_options(),
                0,
            )
            .await
    });
    gate.entered.notified().await;

    // Act
    gate.release.notify_one();
    let result = task.await.expect("waiter");
    database.reservation_observer = Arc::new(());
    let replacement = database
        .begin_turn(
            Arc::new(database.clone()),
            "session",
            "replacement",
            &turn_options(),
            0,
        )
        .await
        .expect("replacement");

    // Assert
    assert!(
        matches!(result, Err(SessionError::InvalidData { reason }) if reason.contains("reservation task failed"))
    );
    assert_eq!(replacement.guard.owner.turn_position, 0);
}

#[tokio::test]
async fn expired_acquisition_acknowledgement_never_returns_an_executable_turn() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 100_000)
        .await
        .expect("session");
    let gate = Arc::new(AcquisitionGate {
        entered: Notify::new(),
        release: Notify::new(),
    });
    let mut gated = database.clone();
    gated.reservation_observer = gate.clone();
    let task = tokio::spawn(async move {
        gated
            .begin_turn(
                Arc::new(gated.clone()),
                "session",
                "expired",
                &turn_options(),
                0,
            )
            .await
    });
    gate.entered.notified().await;

    // Act
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::time::resume();
    gate.release.notify_one();
    let result = task.await.expect("acquisition");
    let replacement = database
        .begin_turn(
            Arc::new(database.clone()),
            "session",
            "replacement",
            &turn_options(),
            0,
        )
        .await
        .expect("replacement");

    // Assert
    assert!(matches!(result, Err(SessionError::OwnershipLost { .. })));
    assert_eq!(replacement.guard.owner.turn_position, 1);
}
