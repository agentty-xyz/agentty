use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tempfile::tempdir;

use crate::TurnError;
use crate::gated_store_test::{GatedStore, PauseAt};
use crate::input::TurnInput;
use crate::model::{ModelError, ModelMessage};
use crate::reservation::TURN_LEASE_SECONDS;
use crate::session::tests::support::{
    acquire, allow_interrupts, reject_interrupts, schema, turn_options,
};
use crate::session::{Database, NewSession, SessionError, StoreIdentity};
use crate::store::{SessionStore, WriteStatus};

#[tokio::test]
async fn expired_and_wrong_owners_cannot_mutate_but_existing_writes_can_settle() {
    // Arrange
    let directory = tempdir().expect("directory");
    let path = directory.path().join("session.db");
    let now = Arc::new(AtomicI64::new(10));
    let clock = now.clone();
    let database =
        Database::open_with_timestamp_source(&path, Arc::new(move || clock.load(Ordering::SeqCst)))
            .await
            .expect("database");
    database
        .create_session(&NewSession::new("session", schema()), None, 100_000)
        .await
        .expect("session");
    let mut acquired = database
        .begin_turn(
            Arc::new(database.clone()),
            "session",
            &TurnInput::from("prompt"),
            &turn_options(),
            0,
        )
        .await
        .expect("turn");
    acquired.guard.disarm();
    let owner = acquired.guard.owner().clone();
    let journal = acquired.guard.write_journal();
    let intent = journal
        .intent("write", Path::new("repo"), "file", None, b"result")
        .await
        .expect("intent");
    let mut wrong = owner.clone();
    wrong.token = vec![0];
    let mut foreign = owner.clone();
    foreign.database = StoreIdentity::temporary();
    now.store(10 + TURN_LEASE_SECONDS, Ordering::SeqCst);

    // Act
    for candidate in [&owner, &wrong, &foreign] {
        assert!(matches!(
            database.renew(candidate).await,
            Err(SessionError::OwnershipLost { .. })
        ));
        assert!(matches!(
            SessionStore::complete_turn(
                &database,
                candidate,
                &[ModelMessage::Assistant("discard".into())],
                None
            )
            .await,
            Err(SessionError::OwnershipLost { .. })
        ));
        assert!(matches!(
            SessionStore::fail_turn(
                &database,
                candidate,
                &TurnError::Model(ModelError::InvalidResponse)
            )
            .await,
            Err(SessionError::OwnershipLost { .. })
        ));
        assert!(matches!(
            database
                .write_intent(candidate, "late", Path::new("repo"), "file", None, b"late")
                .await,
            Err(SessionError::OwnershipLost { .. })
        ));
    }
    assert!(database.finish_write(&wrong, intent, true).await.is_err());
    assert!(database.finish_write(&foreign, intent, true).await.is_err());
    assert!(database.interrupt(&foreign).await.is_err());
    database
        .interrupt(&owner)
        .await
        .expect("interrupt expired owner");
    journal
        .finish(intent, true)
        .await
        .expect("settle admitted write");
    let reopened = Database::open(&path).await.expect("reopen");
    let writes = reopened.load_writes("session").await.expect("writes");
    let loaded = reopened.load_session("session").await.expect("history");

    // Assert
    assert_eq!(database.identity(), reopened.identity());
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].status, WriteStatus::Applied);
    assert_eq!(loaded.turns, Vec::<Vec<ModelMessage>>::new());
}

#[tokio::test]
async fn failed_drop_cleanup_remains_registered_for_owner_scoped_recovery() {
    // Arrange
    let (store, acquired) = GatedStore::fixture(PauseAt::Completion).await;
    reject_interrupts(&store.database).await;
    let owner = acquired.guard.owner().clone();
    drop(acquired);
    store.interrupted.notified().await;
    allow_interrupts(&store.database).await;
    let direct = store
        .begin_turn(
            store.clone(),
            "session",
            &TurnInput::from("direct"),
            &turn_options(),
            0,
        )
        .await;

    // Act
    let replacement = acquire(store.clone(), "session", "replacement")
        .await
        .expect("recover owner");

    // Assert
    assert!(matches!(direct, Err(SessionError::Busy { .. })));
    assert_eq!(
        replacement.guard.owner().turn_position,
        owner.turn_position + 1
    );
}
