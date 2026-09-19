use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tempfile::tempdir;
use tokio::sync::Notify;

use super::support::{AcquisitionGate, CommitGate, GatedStore, PauseAt};
use crate::WriteStatus;
use crate::session::tests::support::{schema, turn_options};
use crate::session::{Database, NewSession, ReservationObserver, SessionError};
use crate::store::SessionStore;

#[tokio::test]
async fn acquisition_rejects_a_different_backing_store_before_reserving() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 100_000)
        .await
        .expect("session");
    let other = Arc::new(Database::open_in_memory().await.expect("other store"));

    // Act
    let result = database
        .begin_turn(other, "session", "prompt", &turn_options(), 0)
        .await;
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_turn")
        .fetch_one(&database.pool)
        .await
        .expect("reservation count");

    // Assert
    assert!(matches!(result, Err(SessionError::InvalidData { reason })
        if reason == "acquisition store has a different backing identity"));
    assert_eq!(count, 0);
}

#[tokio::test]
async fn acquired_journal_and_cleanup_dispatch_through_the_decorator() {
    // Arrange
    let (store, acquired) = GatedStore::fixture(PauseAt::Completion).await;
    let directory = tempdir().expect("workspace");
    let journal = acquired.guard.write_journal();

    // Act
    let intent = journal
        .intent("call", directory.path(), "file.txt", None, b"written")
        .await
        .expect("intent");
    journal.finish(intent, true).await.expect("outcome");
    drop(acquired);
    tokio::time::timeout(Duration::from_secs(2), store.interrupted.notified())
        .await
        .expect("decorator observes cleanup");
    let writes = store.load_writes("session").await.expect("writes");
    let status = sqlx::query_scalar::<_, String>("SELECT status FROM session_turn")
        .fetch_one(&store.database.pool)
        .await
        .expect("turn status");

    // Assert
    assert_eq!(store.write_intents.load(Ordering::SeqCst), 1);
    assert_eq!(store.write_outcomes.load(Ordering::SeqCst), 1);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].status, WriteStatus::Applied);
    assert_eq!(status, "interrupted");
}

#[tokio::test]
async fn cancelled_acquisition_retains_the_decorator_before_and_after_commit() {
    for before_commit in [true, false] {
        // Arrange
        let commit = Arc::new(CommitGate {
            entered: Notify::new(),
            fail: false,
            release: Notify::new(),
        });
        let acknowledgement = Arc::new(AcquisitionGate {
            entered: Notify::new(),
            release: Notify::new(),
        });
        let (observer, entered, release): (Arc<dyn ReservationObserver>, _, _) = if before_commit {
            (commit.clone(), &commit.entered, &commit.release)
        } else {
            (
                acknowledgement.clone(),
                &acknowledgement.entered,
                &acknowledgement.release,
            )
        };
        let mut database = Database::open_in_memory().await.expect("database");
        database.reservation_observer = observer;
        let store = Arc::new(GatedStore::new(database, PauseAt::Completion));
        let backend: Arc<dyn SessionStore> = store.clone();
        backend
            .create_session(&NewSession::new("session", schema()), None, 100_000)
            .await
            .expect("session");
        let task = tokio::spawn(async move {
            backend
                .begin_turn(
                    Arc::clone(&backend),
                    "session",
                    "prompt",
                    &turn_options(),
                    0,
                )
                .await
        });
        entered.notified().await;

        // Act
        task.abort();
        assert!(task.await.err().expect("cancelled waiter").is_cancelled());
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), store.interrupted.notified())
            .await
            .expect("retained guard dispatches cleanup through decorator");
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM session_turn")
            .fetch_one(&store.database.pool)
            .await
            .expect("turn status");

        // Assert
        assert_eq!(status, "interrupted");
        assert_eq!(store.renewals.load(Ordering::SeqCst), 0);
    }
}
