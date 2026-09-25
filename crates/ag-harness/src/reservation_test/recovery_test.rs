use std::sync::Arc;

use crate::effect::Effects;
use crate::reservation::{self, abandon, recover_session, retained_store};
use crate::session::AcquiredTurn;
use crate::store::{MemoryStore, NewSession, SessionStore};
use crate::store_conformance_test::{options, schema, stores};
use crate::{SessionError, TurnInput};

async fn acquire(store: &Arc<dyn SessionStore>, prompt: &str) -> AcquiredTurn {
    reservation::acquire(
        Arc::clone(store),
        ("session".into(), 0),
        TurnInput::from(prompt),
        options(),
        None,
        Effects::default(),
    )
    .await
    .expect("turn should be acquired")
}

/// Reserves directly through the store, without process-local admission.
async fn reserve(store: &Arc<dyn SessionStore>, prompt: &str) -> AcquiredTurn {
    store
        .begin_turn(
            Arc::clone(store),
            "session",
            &TurnInput::from(prompt),
            &options(),
            0,
        )
        .await
        .expect("turn should be reserved")
}

async fn create_session(store: &Arc<dyn SessionStore>) {
    store
        .create_session(&NewSession::new("session", schema()), None, 100_000)
        .await
        .expect("session should be created");
}

#[tokio::test]
async fn acquisition_recovers_abandoned_owners_for_every_store() {
    for store in stores().await {
        // Arrange
        create_session(&store).await;
        let mut abandoned = reserve(&store, "abandoned").await;
        abandoned.guard.disarm();
        let owner = abandoned.owner().clone();
        drop(abandoned);
        abandon(owner.clone(), Arc::clone(&store));
        let direct = store
            .begin_turn(
                Arc::clone(&store),
                "session",
                &TurnInput::from("direct"),
                &options(),
                0,
            )
            .await;

        // Act
        let replacement = acquire(&store, "replacement").await;

        // Assert
        assert!(matches!(direct, Err(SessionError::Busy { .. })));
        assert_eq!(
            replacement.owner().turn_position(),
            owner.turn_position() + 1
        );
        assert!(matches!(
            store.renew(&owner).await,
            Err(SessionError::OwnershipLost { .. })
        ));
        assert!(retained_store(&owner).is_none());
    }
}

#[tokio::test]
async fn recovering_an_already_recovered_owner_is_inert() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    create_session(&store).await;
    let mut abandoned = reserve(&store, "abandoned").await;
    abandoned.guard.disarm();
    let owner = abandoned.owner().clone();
    drop(abandoned);
    abandon(owner.clone(), Arc::clone(&store));
    reservation::recover_owner(&owner)
        .await
        .expect("retained owner should be recovered");
    let successor = acquire(&store, "successor").await;

    // Act
    reservation::recover_owner(&owner)
        .await
        .expect("already recovered owner is inert");

    // Assert
    assert!(store.renew(successor.owner()).await.is_ok());
}

#[tokio::test]
async fn abandoned_owners_are_scoped_to_their_store() {
    // Arrange
    let first: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let second: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let mut owners = Vec::new();
    for store in [&first, &second] {
        create_session(store).await;
        let mut abandoned = reserve(store, "abandoned").await;
        abandoned.guard.disarm();
        let owner = abandoned.owner().clone();
        drop(abandoned);
        abandon(owner.clone(), Arc::clone(store));
        owners.push(owner);
    }

    // Act
    recover_session(second.identity(), "session")
        .await
        .expect("second store should recover");

    // Assert
    assert!(first.renew(&owners[0]).await.is_ok());
    assert!(retained_store(&owners[0]).is_some());
    assert!(matches!(
        second.renew(&owners[1]).await,
        Err(SessionError::OwnershipLost { .. })
    ));
    assert!(retained_store(&owners[1]).is_none());
}
