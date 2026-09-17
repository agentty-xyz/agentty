use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::Notify;

use super::support::{GatedStore, PauseAt};
use crate::TurnError;
use crate::model::{ModelError, ModelMessage};
use crate::session::SessionError;
use crate::session::tests::support::turn_options;
use crate::store::SessionStore;

#[tokio::test]
async fn stalled_renewal_cannot_extend_the_last_confirmed_deadline() {
    // Arrange
    let (store, mut acquired) = GatedStore::fixture(PauseAt::Renewal).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(100)).await;
    store.entered.notified().await;
    let started = Arc::new(Notify::new());
    let ready = started.clone();
    let waiter = tokio::spawn(async move {
        ready.notify_one();
        acquired.guard.ownership_failure().await
    });
    started.notified().await;

    // Act
    tokio::time::advance(Duration::from_secs(200)).await;
    tokio::time::resume();
    let failure = waiter.await.expect("monitor");

    // Assert
    assert!(matches!(failure, SessionError::OwnershipLost { .. }));
    assert_eq!(store.renewals.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn terminal_persistence_is_bounded_and_keeps_cleanup_armed() {
    for phase in [PauseAt::Completion, PauseAt::Failure] {
        // Arrange
        let (store, mut acquired) = GatedStore::fixture(phase).await;
        let task = tokio::spawn(async move {
            let result = if phase == PauseAt::Completion {
                acquired.guard.complete(&[], None, None).await
            } else {
                acquired
                    .guard
                    .fail(&TurnError::Model(ModelError::InvalidResponse))
                    .await
            };

            (result, acquired)
        });
        store.entered.notified().await;

        // Act
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(300)).await;
        tokio::time::resume();
        let (result, acquired) = task.await.expect("finalization");

        // Assert
        assert!(matches!(result, Err(SessionError::OwnershipLost { .. })));
        assert!(acquired.guard.armed);
        assert_eq!(store.renewals.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn completion_acknowledgement_excludes_renewal_and_preserves_success() {
    // Arrange
    let (store, mut acquired) = GatedStore::fixture(PauseAt::CompletionAcknowledgement).await;
    let task = tokio::spawn(async move {
        let result = acquired
            .guard
            .complete(
                &[ModelMessage::Assistant("answer".into())],
                None,
                Some("native"),
            )
            .await;

        (result, acquired)
    });
    store.entered.notified().await;

    // Act
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(101)).await;
    tokio::time::resume();
    store.release.notify_one();
    let (result, acquired) = task.await.expect("finalization");
    let loaded = store.load_session("session").await.expect("history");

    // Assert
    result.expect("completion must win");
    assert!(!acquired.guard.armed);
    assert_eq!(store.renewals.load(Ordering::SeqCst), 0);
    assert_eq!(loaded.provider_session_id.as_deref(), Some("native"));
    assert_eq!(loaded.turns.len(), 1);
}

#[tokio::test]
async fn terminal_acknowledgement_loss_cannot_interrupt_a_successor() {
    // Arrange
    let (store, mut acquired) = GatedStore::fixture(PauseAt::CompletionAcknowledgement).await;
    let owner = acquired.guard.owner.clone();
    let task = tokio::spawn(async move {
        acquired
            .guard
            .complete(&[], None, Some("first-native"))
            .await
    });
    store.entered.notified().await;
    let mut successor = store
        .database
        .begin_turn(
            Arc::new(store.database.clone()),
            "session",
            "next",
            &turn_options(),
        )
        .await
        .expect("successor");
    successor
        .guard
        .complete(&[], None, Some("next-native"))
        .await
        .expect("successor completion");

    // Act
    task.abort();
    assert!(task.await.expect_err("caller cancelled").is_cancelled());
    store
        .interrupt(&owner)
        .await
        .expect("repeat original cleanup");
    let loaded = store.load_session("session").await.expect("session");

    // Assert
    assert_eq!(loaded.provider_session_id.as_deref(), Some("next-native"));
    assert_eq!(loaded.turns.len(), 2);
}

#[tokio::test]
async fn finalization_uses_the_deadline_confirmed_by_a_slow_renewal() {
    // Arrange
    let (store, mut acquired) = GatedStore::fixture(PauseAt::RenewalAndCompletion).await;
    let deadline = Arc::clone(&acquired.guard.deadline);
    let original_deadline = *deadline.lock().expect("confirmed deadline");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(100)).await;
    tokio::time::resume();
    store.entered.notified().await;
    let started = Arc::new(Notify::new());
    let ready = Arc::clone(&started);
    let task = tokio::spawn(async move {
        ready.notify_one();
        let result = acquired
            .guard
            .complete(
                &[ModelMessage::Assistant("answer".into())],
                None,
                Some("native"),
            )
            .await;

        (result, acquired)
    });
    started.notified().await;

    // Act
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(150)).await;
    tokio::time::resume();
    store.release.notify_one();
    store.entered.notified().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(75)).await;
    tokio::time::resume();
    let completion_time = tokio::time::Instant::now();
    store.release.notify_one();
    let (result, acquired) = task.await.expect("finalization");
    let loaded = store.load_session("session").await.expect("session");

    // Assert
    assert!(completion_time > original_deadline);
    assert!(completion_time < *deadline.lock().expect("renewed deadline"));
    result.expect("completion uses the renewed deadline");
    assert!(!acquired.guard.armed);
    assert_eq!(store.renewals.load(Ordering::SeqCst), 1);
    assert_eq!(loaded.provider_session_id.as_deref(), Some("native"));
    assert_eq!(loaded.turns.len(), 1);
}

#[tokio::test]
async fn finalization_waiting_for_renewal_stops_at_the_confirmed_deadline() {
    // Arrange
    let (store, mut acquired) = GatedStore::fixture(PauseAt::Renewal).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(100)).await;
    store.entered.notified().await;
    let started = Arc::new(Notify::new());
    let ready = started.clone();
    let task = tokio::spawn(async move {
        ready.notify_one();
        acquired.guard.complete(&[], None, None).await
    });
    started.notified().await;

    // Act
    tokio::time::advance(Duration::from_secs(200)).await;
    tokio::time::resume();
    let result = task.await.expect("finalization");

    // Assert
    assert!(matches!(result, Err(SessionError::OwnershipLost { .. })));
    assert_eq!(store.renewals.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unacknowledged_renewal_does_not_extend_execution() {
    // Arrange
    let (store, mut acquired) = GatedStore::fixture(PauseAt::RenewalAcknowledgement).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(100)).await;
    tokio::time::resume();
    store.entered.notified().await;
    let started = Arc::new(Notify::new());
    let ready = started.clone();
    let task = tokio::spawn(async move {
        ready.notify_one();
        acquired.guard.ownership_failure().await
    });
    started.notified().await;

    // Act
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(200)).await;
    tokio::time::resume();
    let result = task.await.expect("ownership monitor");

    // Assert
    assert!(matches!(result, SessionError::OwnershipLost { .. }));
    assert_eq!(store.renewals.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn finalization_rechecks_expiry_even_when_the_monitor_has_not_reported() {
    // Arrange
    let (_, mut acquired) = GatedStore::fixture(PauseAt::Completion).await;
    acquired.guard.stop_renewal();
    let (_sender, receiver) = tokio::sync::oneshot::channel();
    acquired.guard.ownership_failure = Some(receiver);
    let exclusive = acquired.guard.finalization.clone().lock_owned().await;
    let started = Arc::new(Notify::new());
    let ready = started.clone();
    let task = tokio::spawn(async move {
        ready.notify_one();
        acquired.guard.complete(&[], None, None).await
    });
    started.notified().await;

    // Act
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::time::resume();
    drop(exclusive);
    let result = task.await.expect("finalization");

    // Assert
    assert!(matches!(result, Err(SessionError::OwnershipLost { .. })));
}

#[tokio::test]
async fn ownership_check_rejects_expiry_before_waiting_for_the_monitor() {
    // Arrange
    let (_, mut acquired) = GatedStore::fixture(PauseAt::Renewal).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::time::resume();

    // Act
    let result = acquired.guard.ownership_failure().await;

    // Assert
    assert!(matches!(result, SessionError::OwnershipLost { .. }));
}
